use ruscello_engine::{ErrorClass, HttpError, MediaError, ResolveError, WriterError};
use ruscello_platform::language::Language;

pub(crate) struct Messages {
    pub usage: &'static str,
    pub restarted: &'static str,
    pub non_resumable: &'static str,
    pub waiting: &'static str,
    pub media_label: &'static str,
    pub formats_heading: &'static str,
    pub playlist_heading: &'static str,
    pub local_api_unavailable: &'static str,
    pub local_api_queued: &'static str,
    cancelled: &'static str,
    changed: &'static str,
    checksum_failed: &'static str,
    checksum_required: &'static str,
    auth: &'static str,
    expired: &'static str,
    temporary_failure: &'static str,
    destination_failure: &'static str,
    failed: &'static str,
    resolution: &'static str,
    verification: &'static str,
    browser: &'static str,
    dlc_key: &'static str,
    sidecar_missing: &'static str,
    media_invalid: &'static str,
}

const EN: Messages = Messages {
    usage: "Usage: ruscello-cli [--language en|it|system] get <url> [-o directory] [-x connections]\n\
    Options: --mirror <url> --checksum <md5|sha1|sha256|crc32:hex> --limit <bytes/s> --ftp-user <user> --ftp-password <password> --format <preset|id> --ytdlp <path> --ffmpeg <path> --progress --quiet --browser\n\
    ruscello-cli inspect <url> [--browser] [--ytdlp <path>] [--ffmpeg <path>]\n\
    ruscello-cli import <file> [--dlc-fallback]\n\
    ruscello-cli queue <url>... [--paused]\n\
    ruscello-cli status",
    restarted: "The previous data could not be resumed. Starting from zero.",
    non_resumable: "Not resumable",
    waiting: "Waiting for network connectivity.",
    media_label: "Media",
    formats_heading: "Formats",
    playlist_heading: "Playlist",
    local_api_unavailable: "No running Ruscello desktop API instance was found in the data directory.",
    local_api_queued: "Queued downloads via local API",
    cancelled: "Download interrupted.",
    changed: "The file on the server changed repeatedly.",
    checksum_failed: "The downloaded file did not match the expected checksum.",
    checksum_required: "Provide an expected checksum to combine mirrors.",
    auth: "The server or proxy requires authentication.",
    expired: "The server rejected the link. It may have expired.",
    temporary_failure: "The transfer failed after retrying.",
    destination_failure: "Ruscello could not write or publish the download in the output directory.",
    failed: "Ruscello could not complete the download.",
    resolution: "Ruscello could not resolve any download links.",
    verification: "The host requires verification. Use --browser to open the verification window.",
    browser: "Ruscello could not use the browser helper. Check that ruscello-webview is installed and its profile is available.",
    dlc_key: "Ruscello could not obtain the container key.",
    sidecar_missing: "This media source requires an external sidecar binary (yt-dlp/ffmpeg). Pass --ytdlp/--ffmpeg or install it from the app Preferences.",
    media_invalid: "The media stream or playlist manifest could not be parsed or is unsupported.",
};

const IT: Messages = Messages {
    usage: "Uso: ruscello-cli [--language en|it|system] get <url> [-o cartella] [-x connessioni]\n\
    Opzioni: --mirror <url> --checksum <md5|sha1|sha256|crc32:hex> --limit <byte/s> --ftp-user <utente> --ftp-password <password> --format <preset|id> --ytdlp <percorso> --ffmpeg <percorso> --progress --quiet --browser\n\
    ruscello-cli inspect <url> [--browser] [--ytdlp <percorso>] [--ffmpeg <percorso>]\n\
    ruscello-cli import <file> [--dlc-fallback]\n\
    ruscello-cli queue <url>... [--paused]\n\
    ruscello-cli status",
    restarted: "Impossibile riprendere i dati precedenti. Il download riparte da zero.",
    non_resumable: "Non riprendibile",
    waiting: "In attesa della connessione di rete.",
    media_label: "Media",
    formats_heading: "Formati",
    playlist_heading: "Playlist",
    local_api_unavailable: "Nessuna istanza locale dell’API Ruscello trovata nella cartella dati.",
    local_api_queued: "Download aggiunti tramite API locale",
    cancelled: "Download interrotto.",
    changed: "Il file sul server è cambiato più volte.",
    checksum_failed: "Il file scaricato non corrisponde al checksum previsto.",
    checksum_required: "Indica il checksum previsto per combinare più mirror.",
    auth: "Il server o il proxy richiede l’autenticazione.",
    expired: "Il server ha rifiutato il link. Potrebbe essere scaduto.",
    temporary_failure: "Il trasferimento non è riuscito dopo vari tentativi.",
    destination_failure: "Ruscello non ha potuto scrivere o pubblicare il download nella cartella di destinazione.",
    failed: "Ruscello non ha potuto completare il download.",
    resolution: "Ruscello non ha potuto risolvere i link di download.",
    verification: "Il servizio richiede una verifica. Usa --browser per aprire la finestra di verifica.",
    browser: "Impossibile usare il browser integrato. Verifica che ruscello-webview sia installato e che il profilo sia disponibile.",
    dlc_key: "Ruscello non ha potuto ottenere la chiave del contenitore.",
    sidecar_missing: "Questa sorgente richiede un binario opzionale (yt-dlp/ffmpeg). Passa --ytdlp/--ffmpeg oppure installalo dalle Impostazioni dell’app.",
    media_invalid: "Il manifest o la playlist multimediale non è valido o non è supportato.",
};

impl Messages {
    pub(crate) fn failure(&self, failure: Option<&HttpError>) -> &'static str {
        match failure {
            Some(HttpError::Cancelled) => self.cancelled,
            Some(HttpError::VerificationRequired) => self.verification,
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

    pub(crate) fn media_failure(&self, failure: &MediaError) -> &'static str {
        if failure.is_cancelled() {
            self.cancelled
        } else if failure.is_low_disk() {
            self.destination_failure
        } else if failure.is_access_denied() {
            self.auth
        } else if failure.is_network() {
            self.temporary_failure
        } else {
            match failure {
                MediaError::ConsentRequired(_) | MediaError::SidecarMissing(_) => {
                    self.sidecar_missing
                }
                MediaError::ChecksumMismatch { .. } => self.checksum_failed,
                MediaError::ManifestParse(_) | MediaError::UnsupportedManifest(_) => {
                    self.media_invalid
                }
                MediaError::Io(_) => self.destination_failure,
                _ => self.failed,
            }
        }
    }

    pub(crate) fn resolve_failure(&self, failure: &ResolveError) -> &'static str {
        match failure {
            ResolveError::Cancelled => self.cancelled,
            ResolveError::VerificationRequired => self.verification,
            ResolveError::Browser(_) => self.browser,
            ResolveError::KeyService => self.dlc_key,
            ResolveError::AccountRequired | ResolveError::Keyring => self.auth,
            _ => self.resolution,
        }
    }
}

pub(crate) fn get(language: Language) -> &'static Messages {
    match language {
        Language::English => &EN,
        Language::Italian => &IT,
    }
}
