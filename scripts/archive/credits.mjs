// Turbo credits of the key that signs Cota's Arweave uploads (decided 14/11/2026: the project pays for the readings).
//
//   node credits.mjs saldo              balance of the signer, in winc and in what it buys today
//   node credits.mjs cartao --usd 20    a card payment page (Stripe, run by ArDrive) whose credits go to the signer
//
// Only the signer's public address leaves this machine. The key itself is read only to know that address
// (keys/archive.json, or $ARCHIVE_KEY with its JSON).

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import bs58 from "bs58";
import { TurboFactory, USD } from "@ardrive/turbo-sdk";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const GIB = 1024 ** 3;

function signerAddress() {
  const json = process.env.ARCHIVE_KEY ?? fs.readFileSync(path.join(ROOT, "keys/archive.json"), "utf8");
  return bs58.encode(Uint8Array.from(JSON.parse(json)).subarray(32));
}

async function main() {
  const [cmd] = process.argv.slice(2);
  const address = signerAddress();
  const turbo = TurboFactory.unauthenticated({ token: "solana" });
  if (cmd === "saldo") {
    const balance = await turbo.getBalance(address);
    const [gib] = await turbo.getUploadCosts({ bytes: [GIB] });
    const winc = BigInt(balance.winc);
    const gibs = Number(winc) / Number(gib.winc);
    console.log(`assinador  ${address}`);
    console.log(`saldo      ${winc} winc ≈ ${(gibs * 1024).toFixed(1)} MiB de envio no preço de hoje`);
    console.log(`           (com o RS inteiro, ~0,4 MiB pagos por dia: ~${Math.floor((gibs * 1024) / 0.4)} dias)`);
    return;
  }
  if (cmd === "cartao") {
    const usd = Number(process.argv[process.argv.indexOf("--usd") + 1]);
    if (!process.argv.includes("--usd") || !(usd >= 5)) throw new Error("informe --usd N (mínimo 5)");
    const session = await turbo.createCheckoutSession({ amount: USD(usd), owner: address });
    const [gib] = await turbo.getUploadCosts({ bytes: [GIB] });
    const mib = (Number(session.winc) / Number(gib.winc)) * 1024;
    console.log(`US$ ${usd} compram ~${mib.toFixed(0)} MiB de envio para ${address}.`);
    console.log(`Abra no navegador e pague com cartão (página da ArDrive/Stripe):\n${session.url}`);
    return;
  }
  console.error("uso: node credits.mjs saldo | cartao --usd N");
  process.exit(2);
}

main().catch((e) => {
  console.error(e.message ?? e);
  process.exit(1);
});
