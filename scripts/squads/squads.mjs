// Operations on the Squads v4 multisig that holds the upgrade authority of program v2.
//
//   node squads.mjs create --time-lock 60          creates the 2-of-3 multisig (once)
//   node squads.mjs status                         members, threshold, wait, pending proposals, program authority
//   node squads.mjs propose-upgrade --buffer ADDR  proposes upgrading program v2 to the code in a buffer
//   node squads.mjs propose-time-lock --seconds N [--allow-lower]
//                                                  proposes a new wait before approved proposals can run
//   node squads.mjs propose-ix --file IX.json [--dry-run]
//                                                  proposes an admin instruction of program v2 (rotate_member,
//                                                  set_authority, new rules), built by
//                                                  `cota-admin --as-multisig VAULT --out IX.json <command>`.
//                                                  It is simulated first, as if the vault had signed: an
//                                                  instruction that would fail is never proposed.
//   node squads.mjs propose-member [--add PUBKEY] [--remove PUBKEY] [--dry-run]
//                                                  proposes changing the multisig's own members (a key handed to
//                                                  another person or institution, or a lost one); adding first, so
//                                                  the multisig never has fewer members than its threshold
//   node squads.mjs approve --index N --member KEYFILE
//   node squads.mjs execute --index N              runs an approved proposal once its wait is over
//
// An upgrade, start to end: `solana program write-buffer target/deploy/cota_v2.so`, then
// `solana program set-buffer-authority BUFFER --new-buffer-authority VAULT`, then propose, two approvals,
// wait, execute. While it waits, the proposal is public: anyone can read what it would change.
//
// Settings in config/upgrade-multisig.json (public addresses only), kept in sync with the chain after every
// `create` and `execute`. Member keys: ~/.config/solana/id.json, keys/upgrader-2.json, keys/upgrader-3.json
// (never committed). The fee payer is `--payer KEYFILE` (default ~/.config/solana/id.json); `status` needs no
// key at all. RPC: $RPC, default devnet.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as multisig from "@sqds/multisig";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_RENT_PUBKEY,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CONFIG = path.join(ROOT, "config/upgrade-multisig.json");
// Program v2. `create` records it in the config; everything else reads it from there.
const DEFAULT_PROGRAM = new PublicKey("FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR");
const LOADER = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
const MEMBER_KEYS = [path.join(os.homedir(), ".config/solana/id.json"), path.join(ROOT, "keys/upgrader-2.json"), path.join(ROOT, "keys/upgrader-3.json")];

const connection = new Connection(process.env.RPC ?? "https://api.devnet.solana.com", "confirmed");
const load = (file) => Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, "utf8"))));
const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
/** A whole number of seconds, or an error: a typo must never become a 0 s wait. */
function seconds(name) {
  const raw = arg(name);
  const n = Number(raw);
  if (raw === undefined || !/^\d+$/.test(raw) || !Number.isSafeInteger(n) || n > 0xffffffff) {
    throw new Error(`--${name} precisa ser um número inteiro de segundos (ex.: 86400), recebi ${JSON.stringify(raw)}`);
  }
  return n;
}
// Loaded only by the commands that sign.
let payerKey;
const payer = () => (payerKey ??= load(arg("payer", MEMBER_KEYS[0])));

async function confirm(sig, what) {
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash();
  const r = await connection.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight }, "confirmed");
  if (r.value.err) throw new Error(`${what}: ${JSON.stringify(r.value.err)}`);
  console.log(`${what}\n  tx https://explorer.solana.com/tx/${sig}?cluster=devnet`);
}

function settings() {
  const s = JSON.parse(fs.readFileSync(CONFIG, "utf8"));
  return { ...s, multisigPda: new PublicKey(s.multisig), vaultPda: new PublicKey(s.vault), programId: new PublicKey(s.program) };
}

const programData = (program) => PublicKey.findProgramAddressSync([program.toBuffer()], LOADER)[0];

/** Rewrites threshold, members and wait in the config from the chain, so the dashboard never shows a stale value. */
async function syncConfig(extra = {}) {
  const s = fs.existsSync(CONFIG) ? JSON.parse(fs.readFileSync(CONFIG, "utf8")) : {};
  const ms = await multisig.accounts.Multisig.fromAccountAddress(connection, new PublicKey(extra.multisig ?? s.multisig));
  const program = new PublicKey(extra.program ?? s.program);
  const out = {
    ...s,
    ...extra,
    program_data: programData(program).toBase58(),
    threshold: ms.threshold,
    time_lock_secs: ms.timeLock,
    members: ms.members.map((m) => m.key.toBase58()),
  };
  fs.writeFileSync(CONFIG, JSON.stringify(out, null, 2) + "\n");
  return ms;
}

async function upgradeAuthority(program) {
  const acc = await connection.getAccountInfo(programData(program));
  // ProgramData: u32 tag (3) | u64 slot | Option<Pubkey> authority
  return acc.data[12] === 1 ? new PublicKey(acc.data.subarray(13, 45)).toBase58() : null;
}

async function create() {
  if (fs.existsSync(CONFIG)) throw new Error(`${CONFIG} já existe`);
  const createKey = Keypair.generate();
  const [multisigPda] = multisig.getMultisigPda({ createKey: createKey.publicKey });
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });
  const [programConfigPda] = multisig.getProgramConfigPda({});
  const { treasury } = await multisig.accounts.ProgramConfig.fromAccountAddress(connection, programConfigPda);
  const members = MEMBER_KEYS.map((f) => ({ key: load(f).publicKey, permissions: multisig.types.Permissions.all() }));
  const timeLock = seconds("time-lock");
  const sig = await multisig.rpc.multisigCreateV2({
    connection,
    treasury,
    createKey,
    creator: payer(),
    multisigPda,
    configAuthority: null, // changes to members or the wait also need 2 of 3 and the wait
    threshold: 2,
    members,
    timeLock,
    rentCollector: null,
  });
  await confirm(sig, `multisig 2-de-3 criada: ${multisigPda.toBase58()} (cofre ${vaultPda.toBase58()}, espera ${timeLock} s)`);
  await syncConfig({
    program: DEFAULT_PROGRAM.toBase58(),
    multisig: multisigPda.toBase58(),
    vault: vaultPda.toBase58(),
    squads_program: multisig.PROGRAM_ID.toBase58(),
    created_tx: sig,
  });
}

async function nextIndex(multisigPda) {
  const ms = await multisig.accounts.Multisig.fromAccountAddress(connection, multisigPda);
  return BigInt(ms.transactionIndex.toString()) + 1n;
}

async function proposeUpgrade() {
  const { multisigPda, vaultPda, programId } = settings();
  if (!arg("buffer")) throw new Error("--buffer é obrigatório");
  const buffer = new PublicKey(arg("buffer"));
  // The program account must already have room for the new code, or the upgrade fails only at execution, a day
  // later. Buffer = 37-byte header + code; ProgramData = 45-byte header + reserved space.
  const [buf, pd] = await Promise.all([connection.getAccountInfo(buffer), connection.getAccountInfo(programData(programId))]);
  if (!buf || !pd) throw new Error("buffer ou programa não encontrado");
  const code = buf.data.length - 37;
  const room = pd.data.length - 45;
  if (code > room) {
    throw new Error(
      `o código novo (${code} bytes) não cabe no espaço do programa (${room}); antes, rode: solana program extend ${programId.toBase58()} ${code - room + 4096}`,
    );
  }
  // BPF upgradeable loader, instruction 3 = Upgrade.
  const upgrade = new TransactionInstruction({
    programId: LOADER,
    data: Buffer.from([3, 0, 0, 0]),
    keys: [
      { pubkey: programData(programId), isSigner: false, isWritable: true },
      { pubkey: programId, isSigner: false, isWritable: true },
      { pubkey: buffer, isSigner: false, isWritable: true },
      { pubkey: payer().publicKey, isSigner: false, isWritable: true }, // receives the buffer's lamports back
      { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
      { pubkey: vaultPda, isSigner: true, isWritable: false },
    ],
  });
  await propose(multisigPda, async (index) =>
    multisig.rpc.vaultTransactionCreate({
      connection,
      feePayer: payer(),
      multisigPda,
      transactionIndex: index,
      creator: payer().publicKey,
      vaultIndex: 0,
      ephemeralSigners: 0,
      transactionMessage: new TransactionMessage({
        payerKey: vaultPda,
        recentBlockhash: (await connection.getLatestBlockhash()).blockhash,
        instructions: [upgrade],
      }),
      memo: `upgrade ${programId.toBase58()} from buffer ${buffer.toBase58()}`,
    }),
  );
}

async function proposeTimeLock() {
  const { multisigPda } = settings();
  const newLock = seconds("seconds");
  const ms = await multisig.accounts.Multisig.fromAccountAddress(connection, multisigPda);
  if (newLock < ms.timeLock && !process.argv.includes("--allow-lower")) {
    throw new Error(`a espera atual é ${ms.timeLock} s; para reduzir para ${newLock} s, repita com --allow-lower`);
  }
  await propose(multisigPda, (index) =>
    multisig.rpc.configTransactionCreate({
      connection,
      feePayer: payer(),
      multisigPda,
      transactionIndex: index,
      creator: payer().publicKey,
      actions: [{ __kind: "SetTimeLock", newTimeLock: newLock }],
      memo: `time lock ${newLock} s`,
    }),
  );
}

/** Proposes an admin instruction of program v2, from the JSON written by `cota-admin --as-multisig`. */
async function proposeIx() {
  const { multisigPda, vaultPda, programId } = settings();
  if (!arg("file")) throw new Error("--file é obrigatório (gerado por cota-admin --as-multisig VAULT --out ARQUIVO)");
  const spec = JSON.parse(fs.readFileSync(arg("file"), "utf8"));
  // The file is checked before anything goes on-chain: only our program, and only our vault as signer.
  if (spec.program_id !== programId.toBase58()) throw new Error(`a instrução é do programa ${spec.program_id}, não do Cota (${programId.toBase58()})`);
  const signers = spec.accounts.filter((a) => a.is_signer).map((a) => a.pubkey);
  if (signers.length !== 1 || signers[0] !== vaultPda.toBase58()) {
    throw new Error(`o único assinante deve ser o cofre da multisig ${vaultPda.toBase58()}; veio: ${signers.join(", ") || "nenhum"}`);
  }
  const ix = new TransactionInstruction({
    programId,
    data: Buffer.from(spec.data_hex, "hex"),
    keys: spec.accounts.map((a) => ({ pubkey: new PublicKey(a.pubkey), isSigner: a.is_signer, isWritable: a.is_writable })),
  });
  console.log(`instrução: ${spec.description}`);

  // An instruction that creates an account (a new rule) makes the vault pay its rent when it runs.
  if (spec.accounts.some((a) => a.pubkey === SystemProgram.programId.toBase58())) {
    const balance = (await connection.getBalance(vaultPda)) / LAMPORTS_PER_SOL;
    console.log(`  cria uma conta: o aluguel sai do cofre, que tem ${balance} SOL`);
    if (balance < 0.01) throw new Error(`o cofre ${vaultPda.toBase58()} precisa de saldo para o aluguel; transfira ~0,01 SOL para ele antes`);
  }

  // Simulation as if the vault had signed (signatures are not checked in a simulation): the program runs the
  // instruction against the current state and nothing is written. An instruction that would fail at execution,
  // after the 24-hour wait, fails here instead.
  const message = new TransactionMessage({
    payerKey: payer().publicKey,
    recentBlockhash: (await connection.getLatestBlockhash()).blockhash,
    instructions: [ix],
  }).compileToV0Message();
  const sim = await connection.simulateTransaction(new VersionedTransaction(message), { sigVerify: false, replaceRecentBlockhash: true });
  const logs = (sim.value.logs ?? []).filter((l) => l.includes("Program log:") || l.includes("failed") || l.includes("success"));
  if (sim.value.err) {
    throw new Error(`a simulação falhou: ${JSON.stringify(sim.value.err)}\n${logs.join("\n")}`);
  }
  console.log(`  simulação: ok (${sim.value.unitsConsumed} unidades de computação)`);
  logs.forEach((l) => console.log(`    ${l}`));
  if (process.argv.includes("--dry-run")) {
    console.log("--dry-run: nada foi proposto");
    return;
  }

  await propose(multisigPda, async (index) =>
    multisig.rpc.vaultTransactionCreate({
      connection,
      feePayer: payer(),
      multisigPda,
      transactionIndex: index,
      creator: payer().publicKey,
      vaultIndex: 0,
      ephemeralSigners: 0,
      transactionMessage: new TransactionMessage({
        payerKey: vaultPda,
        recentBlockhash: (await connection.getLatestBlockhash()).blockhash,
        instructions: [ix],
      }),
      memo: spec.description.slice(0, 200),
    }),
  );
}

/** Proposes adding and/or removing members of the multisig itself. */
async function proposeMember() {
  const { multisigPda } = settings();
  const add = arg("add");
  const remove = arg("remove");
  if (!add && !remove) throw new Error("informe --add PUBKEY e/ou --remove PUBKEY");
  const ms = await multisig.accounts.Multisig.fromAccountAddress(connection, multisigPda);
  const members = ms.members.map((m) => m.key.toBase58());
  const actions = [];
  // Adding before removing: at no point does the multisig have fewer members than its threshold.
  if (add) {
    const key = new PublicKey(add);
    if (members.includes(key.toBase58())) throw new Error(`${key.toBase58()} já é membro`);
    actions.push({ __kind: "AddMember", newMember: { key, permissions: multisig.types.Permissions.all() } });
  }
  if (remove) {
    const key = new PublicKey(remove);
    if (!members.includes(key.toBase58())) throw new Error(`${key.toBase58()} não é membro (membros: ${members.join(", ")})`);
    actions.push({ __kind: "RemoveMember", oldMember: key });
  }
  const after = members.length + (add ? 1 : 0) - (remove ? 1 : 0);
  if (after < ms.threshold) throw new Error(`ficariam ${after} membros para um limiar de ${ms.threshold}`);
  console.log(`membros hoje (${ms.threshold} de ${members.length}): ${members.join(", ")}`);
  if (add) console.log(`  + ${add}`);
  if (remove) console.log(`  − ${remove}`);
  console.log(`depois: ${ms.threshold} de ${after}`);
  // Squads marks every proposal older than an executed config change as stale: approve or execute those first.
  console.log("atenção: quando esta mudança for executada, as propostas abertas antes dela deixam de valer (regra do Squads)");
  if (process.argv.includes("--dry-run")) {
    console.log("--dry-run: nada foi proposto");
    return;
  }
  const memo = [add && `add ${add}`, remove && `remove ${remove}`].filter(Boolean).join(", ");
  await propose(multisigPda, (index) =>
    multisig.rpc.configTransactionCreate({ connection, feePayer: payer(), multisigPda, transactionIndex: index, creator: payer().publicKey, actions, memo }),
  );
}

/** Creates the transaction and its proposal; the creator (first member) approves it right away. */
async function propose(multisigPda, createTx) {
  const index = await nextIndex(multisigPda);
  await confirm(await createTx(index), `transação ${index} criada`);
  await confirm(
    await multisig.rpc.proposalCreate({ connection, feePayer: payer(), creator: payer(), multisigPda, transactionIndex: index }),
    `proposta ${index} aberta`,
  );
  await confirm(
    await multisig.rpc.proposalApprove({ connection, feePayer: payer(), member: payer(), multisigPda, transactionIndex: index }),
    `proposta ${index}: 1ª aprovação (${payer().publicKey.toBase58()})`,
  );
}

async function approve() {
  const { multisigPda } = settings();
  if (!arg("member") || !arg("index")) throw new Error("--member e --index são obrigatórios");
  const member = load(arg("member"));
  const index = BigInt(arg("index"));
  await confirm(
    await multisig.rpc.proposalApprove({ connection, feePayer: payer(), member, multisigPda, transactionIndex: index }),
    `proposta ${index} aprovada por ${member.publicKey.toBase58()}`,
  );
}

async function execute() {
  const { multisigPda } = settings();
  if (!arg("index")) throw new Error("--index é obrigatório");
  const index = BigInt(arg("index"));
  const [txPda] = multisig.getTransactionPda({ multisigPda, index });
  const info = await connection.getAccountInfo(txPda);
  if (!info) throw new Error(`a transação ${index} não existe (índice errado, ou já executada e fechada)`);
  const isConfig = info.data.subarray(0, 8).equals(Buffer.from(multisig.accounts.configTransactionDiscriminator));
  const sig = isConfig
    ? await multisig.rpc.configTransactionExecute({ connection, feePayer: payer(), multisigPda, transactionIndex: index, member: payer(), rentPayer: payer() })
    : await multisig.rpc.vaultTransactionExecute({ connection, feePayer: payer(), multisigPda, transactionIndex: index, member: payer().publicKey });
  await confirm(sig, `proposta ${index} executada`);
  await syncConfig();
}

async function status() {
  const { multisigPda, vaultPda, programId } = settings();
  const ms = await multisig.accounts.Multisig.fromAccountAddress(connection, multisigPda);
  console.log(`multisig    ${multisigPda.toBase58()}`);
  console.log(`cofre       ${vaultPda.toBase58()}`);
  console.log(`limiar      ${ms.threshold} de ${ms.members.length}`);
  console.log(`espera      ${ms.timeLock} s`);
  ms.members.forEach((m) => console.log(`  membro    ${m.key.toBase58()}`));
  const last = Number(ms.transactionIndex.toString());
  for (let i = 1; i <= last; i++) {
    const [p] = multisig.getProposalPda({ multisigPda, transactionIndex: BigInt(i) });
    try {
      const pr = await multisig.accounts.Proposal.fromAccountAddress(connection, p);
      console.log(`  proposta ${i}: ${pr.status.__kind}, ${pr.approved.length} aprovação(ões)`);
    } catch {
      console.log(`  proposta ${i}: sem conta (já executada e fechada, ou só criada)`);
    }
  }
  const auth = await upgradeAuthority(programId);
  console.log(`programa    ${programId.toBase58()}: autoridade de upgrade ${auth ?? "nenhuma (congelado)"}${auth === vaultPda.toBase58() ? " = cofre da multisig" : ""}`);
}

const commands = { create, status, "propose-upgrade": proposeUpgrade, "propose-time-lock": proposeTimeLock, "propose-ix": proposeIx, "propose-member": proposeMember, approve, execute };
const cmd = commands[process.argv[2]];
if (!cmd) {
  console.error(`uso: node squads.mjs ${Object.keys(commands).join("|")} [opções]`);
  process.exit(2);
}
cmd().catch((e) => {
  // The message is enough to act on; the stack trace only with DEBUG=1.
  const text = e instanceof Error && !process.env.DEBUG ? `erro: ${e.message}` : e;
  console.error(e.logs ? `${text}\n${e.logs.join("\n")}` : text);
  process.exit(1);
});
