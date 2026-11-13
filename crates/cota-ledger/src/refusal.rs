use std::fmt;

/// Why an entry or a checkpoint was refused. The messages are in Portuguese: the verifier shows them to the public.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Malformed,
    // signatures
    UnknownCommittee,
    WrongEpoch { expected: u32, got: u32 },
    SignaturesNotSorted,
    UnknownMember(u16),
    BadSignature(u16),
    ThresholdNotMet { have: usize, need: u16 },
    // committee
    InvalidCommittee,
    NoGenesis,
    GenesisAgain,
    GenesisMismatch,
    ChangePending,
    NoChange,
    WrongAnnouncement,
    // structure
    TextTooLong,
    Duplicate,
    UnknownStream,
    StreamExists,
    RuleExists,
    UnknownRule,
    UnknownSource,
    StreamMismatch,
    // batches and records
    BatchOutOfSequence,
    BrokenHashChain,
    InvalidWindow,
    NoBatchYet,
    StaleBatch,
    InvalidDegradation,
    StaleForecastBase,
    TargetAlreadyAnchored,
    InvalidForecast,
    InvalidPublicAlert,
    InvalidOutlook,
    AlreadyIssued,
    InvalidExternalForecast,
    InvalidCorrection,
    InvalidRule,
    // checkpoints
    CheckpointMismatch,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Refusal::*;
        match self {
            Malformed => write!(f, "entrada malformada (não é uma entrada da v3)"),
            UnknownCommittee => write!(f, "comitê desconhecido"),
            WrongEpoch { expected, got } => write!(f, "assinada pelo comitê da época {got}; a época em vigor é {expected}"),
            SignaturesNotSorted => write!(f, "assinaturas fora de ordem ou repetidas"),
            UnknownMember(m) => write!(f, "assinatura de uma posição ({m}) que não existe no comitê"),
            BadSignature(m) => write!(f, "a assinatura do membro {m} não confere com o conteúdo"),
            ThresholdNotMet { have, need } => write!(f, "{have} assinatura(s); o comitê exige {need}"),
            InvalidCommittee => write!(f, "comitê inválido: o limiar precisa ser maioria (mais da metade) e as chaves, distintas"),
            NoGenesis => write!(f, "o livro ainda não tem o registro de abertura"),
            GenesisAgain => write!(f, "o livro já tem um registro de abertura"),
            GenesisMismatch => write!(f, "o registro de abertura não confere com o último estado da v2"),
            ChangePending => write!(f, "já há uma mudança de comitê pendente"),
            NoChange => write!(f, "a mudança repete o comitê atual"),
            WrongAnnouncement => write!(f, "mudança de comitê com época ou data de anúncio errada"),
            TextTooLong => write!(f, "texto vazio ou maior que o permitido"),
            Duplicate => write!(f, "cópia de uma entrada já registrada"),
            UnknownStream => write!(f, "região (stream) inexistente"),
            StreamExists => write!(f, "essa região (stream) já existe"),
            RuleExists => write!(f, "essa regra ou fonte já existe (regras não mudam)"),
            UnknownRule => write!(f, "regra inexistente"),
            UnknownSource => write!(f, "fonte de previsão inexistente"),
            StreamMismatch => write!(f, "a região da entrada difere da região da regra ou da fonte"),
            BatchOutOfSequence => write!(f, "lote fora de sequência"),
            BrokenHashChain => write!(f, "o lote não aponta para o lote anterior"),
            InvalidWindow => write!(f, "janela de tempo inválida ou sobreposta ao lote anterior"),
            NoBatchYet => write!(f, "a região ainda não tem lote"),
            StaleBatch => write!(f, "o relatório só vale para o lote mais recente da região"),
            InvalidDegradation => write!(f, "motivo desconhecido ou nota fora de 0 a 1000"),
            StaleForecastBase => write!(f, "a previsão não parte do lote mais recente da região"),
            TargetAlreadyAnchored => write!(f, "o instante previsto já está dentro de um lote gravado"),
            InvalidForecast => write!(f, "quantis fora de ordem (P10 ≤ P50 ≤ P90) ou horizonte zero"),
            InvalidPublicAlert => write!(f, "aviso oficial com validade ou gravidade inválida"),
            InvalidOutlook => write!(f, "perspectiva de chuva fora do dia fechado, janela diferente da regra ou modelos de menos"),
            AlreadyIssued => write!(f, "já há registro para essa região ou fonte nesse horário de emissão"),
            InvalidExternalForecast => write!(f, "previsão de terceiros fora do dia fechado ou com horizonte zero"),
            InvalidCorrection => write!(f, "a entrada corrigida não existe, o motivo é desconhecido ou a correção já foi feita"),
            InvalidRule => write!(f, "regra com valores inválidos"),
            CheckpointMismatch => write!(f, "o ponto de controle não confere com o livro (tamanho, raiz, época ou encadeamento)"),
        }
    }
}

impl std::error::Error for Refusal {}
