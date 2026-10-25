//! Núcleo compartilhado do Cota: leitura canônica, lote assinável e árvore Merkle.
//!
//! Tudo aqui é determinístico: as mesmas leituras produzem os mesmos bytes, a mesma
//! raiz e a mesma mensagem assinada em qualquer nó.

pub mod batch;
pub mod merkle;
pub mod outlook;
pub mod precompile;
pub mod public_alert;
pub mod reading;

pub use batch::{Batch, BatchError, BatchHeader, BATCH_DOMAIN};
pub use merkle::Hash;
pub use outlook::{OutlookHeader, OUTLOOK_DOMAIN};
pub use public_alert::{PublicAlertHeader, PUBLIC_ALERT_DOMAIN};
pub use reading::{Reading, Source, Unit, Variable};
