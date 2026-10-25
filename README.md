<p align="center"><img src="docs/logo-banner.png" alt="Cota" width="640"></p>

<p align="center"><strong>Tamper-proof, publicly verifiable flood data, forecasts and warnings on Solana.</strong><br>
<a href="https://cota-eight.vercel.app">cota-eight.vercel.app</a> · <a href="https://cota-eight.vercel.app/?lang=en">English</a> · <a href="https://cota-eight.vercel.app/?lang=pt">Português</a></p>

> Historical replay / research prototype. Cota does **not** issue official warnings.
> Official alerts in Brazil: **SMS 40199**, Defesa Civil.

## About

In May 2024 the Guaíba river in southern Brazil rose higher than ever recorded and Porto Alegre went underwater. More than 180
people died in Rio Grande do Sul. Afterwards, everyone asked the same questions: who knew? Who warned us, and when?

Those questions were hard to answer. Forecast websites overwrite old forecasts, official warnings go offline when they expire, and
spreadsheets can be edited later. Cota is the record that was missing: a river diary nobody can erase. Every day it writes what was
known about the river to Solana (devnet), where anyone can read it and nobody can change it.

- **Three oracles, two signatures.** A blockchain can't see the weather. Each of Cota's three oracles fetches data on its own from
  INMET rain gauges, GloFAS river discharge and Open-Meteo forecasts, and signs what it saw. The program only records a value that
  at least 2 of the 3 signed byte for byte. If one fails or is tampered with, the other two hold.
- **Forecasts on record before the facts.** A quantile model forecasts the Guaíba's discharge 1 and 2 days ahead with an uncertainty
  band. The program refuses any forecast whose target day is already anchored. In the 2024 replay, the first alert was on record
  **two days before** the river crossed the flood limit.
- **Early warning, 4 to 7 days out.** The Guaíba fills a few days after heavy rain upstream. If three weather models forecast a
  basin-average of 90 mm or more over the next five days, the program raises an alert. The rule was fixed on-chain before it ran,
  so it can't be tuned afterwards. Out of sample (Feb 2024 – Oct 2026) it warned of all three floods, with false alarms that the
  dashboard shows.
- **Official warnings kept.** INMET warnings for Rio Grande do Sul are attested by the committee with a hash of their content, so a
  copy survives after INMET takes them down.
- **Verify in the browser.** The dashboard recomputes Merkle proofs and hashes and checks them against the chain. Tick "Simulate
  tampering" and the proof fails.
- **Parametric vaults.** Funds can be locked with a written condition ("if an early warning is issued for the Guaíba, pay this
  wallet"). When the trigger appears on-chain, the program pays by itself, once per event. Test SOL only.

Cota does not replace civil defense, and it doesn't prove a sensor measured correctly. It proves nobody changed the data, and
when the data existed.

---

## Em português

O Cota é um diário do rio que ninguém consegue apagar: registra na Solana o que se sabia sobre o Guaíba a cada dia, para que
depois de uma enchente dê para provar quem avisou o quê, e quando. O painel tem as duas línguas e um guia "Como ler".
O Cota **não** emite alertas oficiais: para isso, **SMS 40199** e Defesa Civil.
O resto deste README é a documentação técnica.

O Cota registra na Solana (devnet), de forma que qualquer pessoa possa conferir, **quem sabia o quê, e quando**:

- leituras de chuva (INMET) e vazão (GloFAS/Open-Meteo), assinadas por um comitê de 3 oráculos com limiar 2-de-3 e ancoradas como raízes Merkle (RFC 9162), um lote por dia;
- previsões P10/P50/P90 da vazão do Guaíba (LightGBM quantílico, `model_sha256` fixo), que o programa só aceita **antes** de a observação ser ancorada;
- alertas (`AlertReceipt`) quando a P90 cruza um limiar fixado antes;
- avisos oficiais do INMET para o RS, atestados pelo comitê com o SHA-256 do conteúdo (a API do INMET só mostra avisos vigentes);
- **alerta antecipado**: a chuva prevista para os 5 dias seguintes (ECMWF, GFS, ICON; rodada 00Z do dia) nos 6 pontos da bacia,
  atestada pelo comitê logo depois de cada lote; o programa aplica uma regra imutável (≥ 90 mm, calibrada só com 2015–2023) e emite
  `EarlyWarning`. Fora da amostra (fev/2024–out/2026) a regra alertou antes das 3 cheias do período, com 4 a 7 dias de antecedência, e teve 13 dias de alarme falso;
- degradação de fontes (`SourceDegraded`): estação sem dados, sensor travado, nós discordantes;
- **gatilho paramétrico**: cofres (`create_vault`) em que qualquer carteira trava SOL com condições fixas; diante de um gatilho do
  Cota (`EarlyWarning` ou `AlertReceipt`), qualquer conta aciona `trigger_payout` e o programa paga o beneficiário, uma vez por
  gatilho e respeitando a carência. O criador só recupera o saldo depois do vencimento (`close_vault`).

Painel: **https://cota-eight.vercel.app**: replay da cheia de abr–mai/2024 e modo ao vivo, com verificação Merkle e das contas on-chain feita no navegador.

## Estrutura

| Caminho | O quê |
|---|---|
| `programs/cota` | Programa Anchor: comitê, estações, lotes (Ed25519 via precompile), previsões, alertas, avisos oficiais |
| `crates/cota-core` | Formatos canônicos (Borsh), Merkle, mensagem assinada |
| `crates/cota-ingest` | Ingestão INMET (zip anual) e GloFAS (Open-Meteo) |
| `crates/cota-quality` | Regras de sensor, MAD entre nós, índice de confiança C_s |
| `crates/cota-oracle` | Nós, agregador, admin, avisos do INMET (`cota-alerts`), chuva prevista (`cota-outlook`), cofres (`cota-vault`), auditoria (`cota-audit`) |
| `ml/` | Treino, avaliação e previsão (Python/uv); `cota_ml.outlook` calibra a regra de chuva e faz o backtest |
| `dashboard/` | Painel estático (HTML/JS) |
| `data/ledger/` | Estado: cada lote, previsão e aviso ancorado, com links do explorer |
| `scripts/` | `replay.sh`, `live.sh`, `export_dashboard.py`, `next_day.py` |

## Rodar

```sh
cargo test --workspace                     # programa (LiteSVM), oráculos, qualidade
(cd ml && uv run pytest)                   # formatos e ausência de vazamento do futuro
scripts/live.sh all                        # avisos + dia(s) pendente(s) + auditoria + dados do painel
python3 -m http.server -d dashboard 8000   # painel local
```

Replay da cheia: `RPC=… CLUSTER=devnet PAYER=… scripts/replay.sh 2024-04-20 2024-05-25`.
Modelo: `(cd ml && uv run python -m cota_ml.train)` (exige o histórico de `cota-ingest --lenient`).
Regra de chuva e backtest: `(cd ml && uv run python -m cota_ml.outlook)` → `ml/models/outlook-rule.json` (o `rule_sha256` é o
que `cota-admin set-outlook-rule` fixa on-chain) e `ml/reports/outlook-backtest.json`.
Cofre: `cota-vault --rpc … --payer <carteira> create --name … --id 1 --kind early-warning --subject guaiba --beneficiary <chave>
--payout-sol 0.05 --deposit-sol 0.2`; o workflow roda `cota-vault crank` a cada execução.

## Modo ao vivo (GitHub Actions)

`.github/workflows/live.yml` roda a cada 30 min: três jobs de nó (um secret cada) e um agregador que só
leva à cadeia o que tem 2 de 3 assinaturas, faz commit de `data/ledger` e `dashboard/data`.

Secrets do repositório (Settings → Secrets and variables → Actions): `NODE_1_KEY`, `NODE_2_KEY`,
`NODE_3_KEY` e `OPERATOR_KEY`, com o conteúdo de `keys/node-N.json` e `keys/operator.json`.
O operador é a autoridade do comitê (`cota-admin set-authority`); a upgrade authority do programa
não sai da máquina de quem fez o deploy.

## Limites declarados

- Os três oráculos buscam e assinam os dados de forma independente, mas hoje são operados pela mesma pessoa (jobs separados no
  GitHub Actions). O próximo passo é entregá-los a instituições diferentes (Defesa Civil, universidades, ONGs).
- A previsão é da **vazão diária modelada pelo GloFAS**, 1 e 2 dias à frente, não do nível da régua.
- No replay, as transações são de 2026: a prova é da **ordem** (previsão antes da observação), não do calendário de 2024.
- A chuva das estações do INMET chega com ~1 mês de atraso; o modo ao vivo usa só a vazão até haver acesso em tempo real.
- Cofres: na devnet, com SOL de teste. O cofre `replay-2024` aceita os alertas do replay (gravados em 2026 sobre dados de 2024)
  para demonstrar o mecanismo; o `ao-vivo` só aceita gatilhos gravados depois de criado. O programa é compilado com
  `opt-level = "z"` para caber na conta já alocada.
- Alerta antecipado: a regra foi calibrada com chuva **observada** e é aplicada à chuva **prevista** nos pontos das estações. No backtest,
  a previsão usada é um dia mais velha que a do modo ao vivo (sem olhar o futuro). Três cheias em 2,7 anos é amostra pequena: o CSI
  de 0,25 tem incerteza grande. As previsões arquivadas são do Open-Meteo, não do serviço oficial de cada modelo.

Dados: [Weather data by Open-Meteo.com](https://open-meteo.com/) (CC BY 4.0) · GloFAS / Copernicus EMS · INMET.
