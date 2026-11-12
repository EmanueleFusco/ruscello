use ruscello_engine::{ErrorClass, HttpError, WriterError};
use ruscello_platform::language::Language;

pub(crate) struct Messages {
    pub usage: &'static str,
    pub restarted: &'static str,
    pub non_resumable: &'static str,
    pub waiting: &'static str,
    cancelled: &'static str,
    changed: &'static str,
    checksum_failed: &'static str,
    checksum_required: &'static str,
    auth: &'static str,
    expired: &'static str,
    temporary_failure: &'static str,
    destination_failure: &'static str,
    failed: &'static str,
}

const EN: Messages = Messages {
    usage: "Usage: ruscello-cli [--language en|it|system] get <url> [-o directory] [-x connections]\n\
    Options: --mirror <url> --checksum <md5|sha1|sha256|crc32:hex> --limit <bytes/s> --progress --quiet",
    restarted: "The previous data could not be resumed. Starting from zero.",
    non_resumable: "Not resumable",
    waiting: "Waiting for network connectivity.",
    cancelled: "Download interrupted.",
    changed: "The file on the server changed repeatedly.",
    checksum_failed: "The downloaded file did not match the expected checksum.",
    checksum_required: "Provide an expected checksum to combine mirrors.",
    auth: "The server or proxy requires authentication.",
    expired: "The server rejected the link. It may have expired.",
    temporary_failure: "The transfer failed after retrying.",
    destination_failure: "Ruscello could not write or publish the download in the output directory.",
    failed: "Ruscello could not complete the download.",
};

const IT: Messages = Messages {
    usage: "Uso: ruscello-cli [--language en|it|system] get <url> [-o cartella] [-x connessioni]\n\
    Opzioni: --mirror <url> --checksum <md5|sha1|sha256|crc32:hex> --limit <byte/s> --progress --quiet",
    restarted: "Impossibile riprendere i dati precedenti. Il download riparte da zero.",
    non_resumable: "Non riprendibile",
    waiting: "In attesa della connessione di rete.",
    cancelled: "Download interrotto.",
    changed: "Il file sul server è cambiato più volte.",
    checksum_failed: "Il file scaricato non corrisponde al checksum previsto.",
    checksum_required: "Indica il checksum previsto per combinare più mirror.",
    auth: "Il server o il proxy richiede l’autenticazione.",
    expired: "Il server ha rifiutato il link. Potrebbe essere scaduto.",
    temporary_failure: "Il trasferimento non è riuscito dopo vari tentativi.",
    destination_failure: "Ruscello non ha potuto scrivere o pubblicare il download nella cartella di destinazione.",
    failed: "Ruscello non ha potuto completare il download.",
};

impl Messages {
    pub(crate) fn failure(&self, failure: Option<&HttpError>) -> &'static str {
        match failure {
            Some(HttpError::Cancelled) => self.cancelled,
            Some(HttpError::RepresentationChanged) => self.changed,
            Some(HttpError::MirrorChecksumRequired) => self.checksum_required,
            Some(HttpError::Storage(WriterError::ChecksumMismatch)) => self.checksum_failed,
            Some(HttpError::Storage(_)) => self.destination_failure,
            Some(error) => match error.classification() {
                ErrorClass::Authentication => self.auth,
                ErrorClass::ExpiredLink => self.expired,
                ErrorClass::Transient => self.temporary_failure,
                ErrorClass::Permanent => self.failed,
            },
            None => self.destination_failure,
        }
    }
}

pub(crate) fn get(language: Language) -> &'static Messages {
    match language {
        Language::English => &EN,
        Language::Italian => &IT,
    }
}
