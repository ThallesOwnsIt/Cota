use anchor_lang::prelude::*;

#[error_code]
pub enum CotaError {
    #[msg("Somente a autoridade do comitê pode executar esta instrução")]
    Unauthorized,
    #[msg("Comitê inválido: limiar fora de 1..=membros, membros demais ou duplicados")]
    InvalidCommittee,
    #[msg("Membro não encontrado no comitê")]
    MemberNotFound,
    #[msg("Texto maior que o permitido")]
    StringTooLong,
    #[msg("batch_id fora de sequência")]
    BatchOutOfSequence,
    #[msg("prev_batch_root não corresponde ao último lote ancorado")]
    BrokenHashChain,
    #[msg("Janela de tempo inválida ou sobreposta ao lote anterior")]
    InvalidWindow,
    #[msg("A instrução anterior não é uma verificação Ed25519")]
    MissingEd25519Instruction,
    #[msg("Instrução Ed25519 malformada ou apontando para dados de outra instrução")]
    MalformedEd25519Instruction,
    #[msg("Mensagem assinada difere do cabeçalho do lote")]
    MessageMismatch,
    #[msg("Assinatura de uma chave que não pertence ao comitê")]
    UnknownSigner,
    #[msg("Assinaturas distintas insuficientes para o limiar do comitê")]
    ThresholdNotMet,
    #[msg("Nenhum lote ancorado ainda")]
    NoAnchoredBatch,
    #[msg("Relatório de degradação só vale para o lote mais recente")]
    StaleBatch,
    #[msg("Motivo de degradação desconhecido ou índice fora de 0..=1000")]
    InvalidDegradation,
    #[msg("A leitura prevista já está dentro de um lote ancorado")]
    TargetAlreadyAnchored,
    #[msg("Quantis fora de ordem (exige p10 ≤ p50 ≤ p90) ou horizonte nulo")]
    InvalidForecast,
    #[msg("A P90 da previsão não atinge o limiar de alerta")]
    BelowAlertThreshold,
    #[msg("Aviso oficial com vigência inválida")]
    InvalidPublicAlert,
}
