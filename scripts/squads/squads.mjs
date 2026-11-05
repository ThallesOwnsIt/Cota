// Operations on the Squads v4 multisig that holds the upgrade authority of program v2.
//
//   node squads.mjs create --time-lock 60          creates the 2-of-3 multisig (once)
//   node squads.mjs status                         members, threshold, wait, pending proposals, program authority
//   node squads.mjs propose-upgrade --buffer ADDR  proposes upgrading program v2 to the code in a buffer
//   node squads.mjs propose-time-lock --seconds N  proposes a new wait before approved proposals can run
//   node squads.mjs approve --index N --member KEYFILE
//   node squads.mjs execute --index N              runs an approved proposal once its wait is over
//
// An upgrade, start to end: `solana program write-buffer target/deploy/cota_v2.so`, then
// `solana program set-buffer-authority BUFFER --new-buffer-authority VAULT`, then propose, two approvals,
// wait, execute. While it waits, the proposal is public: anyone can read what it would change.
//
// Settings in config/upgrade-multisig.json (public addresses only). Member keys: ~/.config/solana/id.json,
// keys/upgrader-2.json, keys/upgrader-3.json (never committed). RPC: $RPC, default devnet.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as multisig from "@sqds/multisig";
import {
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_RENT_PUBKEY,
  TransactionInstruction,
  TransactionMessage,
} from "@solana/web3.js";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CONFIG = path.join(ROOT, "config/upgrade-multisig.json");
const PROGRAM = new PublicKey("FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR");
const LOADER = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
const MEMBER_KEYS = [path.join(os.homedir(), ".config/solana/id.json"), path.join(ROOT, "keys/upgrader-2.json"), path.join(ROOT, "keys/upgrader-3.json")];

const connection = new Connection(process.env.RPC ?? "https://api.devnet.solana.com", "confirmed");
const load = (file) => Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, "utf8"))));
const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const payer = load(MEMBER_KEYS[0]);

async function confirm(sig, what) {
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash();
  const r = await connection.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight }, "confirmed");
  if (r.value.err) throw new Error(`${what}: ${JSON.stringify(r.value.err)}`);
  console.log(`${what}\n  tx https://explorer.solana.com/tx/${sig}?cluster=devnet`);
}

function settings() {
  const s = JSON.parse(fs.readFileSync(CONFIG, "utf8"));
  return { ...s, multisigPda: new PublicKey(s.multisig), vaultPda: new PublicKey(s.vault) };
}

const programData = () => PublicKey.findProgramAddressSync([PROGRAM.toBuffer()], LOADER)[0];

async function upgradeAuthority() {
  const acc = await connection.getAccountInfo(programData());
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
  const timeLock = Number(arg("time-lock", "0"));
  const sig = await multisig.rpc.multisigCreateV2({
    connection,
    treasury,
    createKey,
    creator: payer,
    multisigPda,
    configAuthority: null, // changes to members or the wait also need 2 of 3 and the wait
    threshold: 2,
    members,
    timeLock,
    rentCollector: null,
  });
  await confirm(sig, `multisig 2-de-3 criada: ${multisigPda.toBase58()} (cofre ${vaultPda.toBase58()}, espera ${timeLock} s)`);
  fs.writeFileSync(
    CONFIG,
    JSON.stringify(
      {
        program: PROGRAM.toBase58(),
        multisig: multisigPda.toBase58(),
        vault: vaultPda.toBase58(),
        threshold: 2,
        members: members.map((m) => m.key.toBase58()),
        squads_program: multisig.PROGRAM_ID.toBase58(),
        created_tx: sig,
      },
      null,
      2,
    ) + "\n",
  );
}

async function nextIndex(multisigPda) {
  const ms = await multisig.accounts.Multisig.fromAccountAddress(connection, multisigPda);
  return BigInt(ms.transactionIndex.toString()) + 1n;
}

async function proposeUpgrade() {
  const { multisigPda, vaultPda } = settings();
  const buffer = new PublicKey(arg("buffer"));
  // BPF upgradeable loader, instruction 3 = Upgrade.
  const upgrade = new TransactionInstruction({
    programId: LOADER,
    data: Buffer.from([3, 0, 0, 0]),
    keys: [
      { pubkey: programData(), isSigner: false, isWritable: true },
      { pubkey: PROGRAM, isSigner: false, isWritable: true },
      { pubkey: buffer, isSigner: false, isWritable: true },
      { pubkey: payer.publicKey, isSigner: false, isWritable: true }, // receives the buffer's lamports back
      { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
      { pubkey: vaultPda, isSigner: true, isWritable: false },
    ],
  });
  await propose(multisigPda, async (index) =>
    multisig.rpc.vaultTransactionCreate({
      connection,
      feePayer: payer,
      multisigPda,
      transactionIndex: index,
      creator: payer.publicKey,
      vaultIndex: 0,
      ephemeralSigners: 0,
      transactionMessage: new TransactionMessage({
        payerKey: vaultPda,
        recentBlockhash: (await connection.getLatestBlockhash()).blockhash,
        instructions: [upgrade],
      }),
      memo: `upgrade ${PROGRAM.toBase58()} from buffer ${buffer.toBase58()}`,
    }),
  );
}

async function proposeTimeLock() {
  const { multisigPda } = settings();
  const seconds = Number(arg("seconds"));
  await propose(multisigPda, (index) =>
    multisig.rpc.configTransactionCreate({
      connection,
      feePayer: payer,
      multisigPda,
      transactionIndex: index,
      creator: payer.publicKey,
      actions: [{ __kind: "SetTimeLock", newTimeLock: seconds }],
      memo: `time lock ${seconds} s`,
    }),
  );
}

/** Creates the transaction and its proposal; the creator (first member) approves it right away. */
async function propose(multisigPda, createTx) {
  const index = await nextIndex(multisigPda);
  await confirm(await createTx(index), `transação ${index} criada`);
  await confirm(
    await multisig.rpc.proposalCreate({ connection, feePayer: payer, creator: payer, multisigPda, transactionIndex: index }),
    `proposta ${index} aberta`,
  );
  await confirm(
    await multisig.rpc.proposalApprove({ connection, feePayer: payer, member: payer, multisigPda, transactionIndex: index }),
    `proposta ${index}: 1ª aprovação (${payer.publicKey.toBase58()})`,
  );
}

async function approve() {
  const { multisigPda } = settings();
  const member = load(arg("member"));
  const index = BigInt(arg("index"));
  await confirm(
    await multisig.rpc.proposalApprove({ connection, feePayer: payer, member, multisigPda, transactionIndex: index }),
    `proposta ${index} aprovada por ${member.publicKey.toBase58()}`,
  );
}

async function execute() {
  const { multisigPda } = settings();
  const index = BigInt(arg("index"));
  const [txPda] = multisig.getTransactionPda({ multisigPda, index });
  const info = await connection.getAccountInfo(txPda);
  const isConfig = info.data.subarray(0, 8).equals(Buffer.from(multisig.accounts.configTransactionDiscriminator));
  const sig = isConfig
    ? await multisig.rpc.configTransactionExecute({ connection, feePayer: payer, multisigPda, transactionIndex: index, member: payer, rentPayer: payer })
    : await multisig.rpc.vaultTransactionExecute({ connection, feePayer: payer, multisigPda, transactionIndex: index, member: payer.publicKey });
  await confirm(sig, `proposta ${index} executada`);
}

async function status() {
  const { multisigPda, vaultPda } = settings();
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
  const auth = await upgradeAuthority();
  console.log(`programa    ${PROGRAM.toBase58()}: autoridade de upgrade ${auth ?? "nenhuma (congelado)"}${auth === vaultPda.toBase58() ? " = cofre da multisig" : ""}`);
}

const commands = { create, status, "propose-upgrade": proposeUpgrade, "propose-time-lock": proposeTimeLock, approve, execute };
const cmd = commands[process.argv[2]];
if (!cmd) {
  console.error(`uso: node squads.mjs ${Object.keys(commands).join("|")} [opções]`);
  process.exit(2);
}
cmd().catch((e) => {
  console.error(e.logs ? `${e.message}\n${e.logs.join("\n")}` : e);
  process.exit(1);
});
