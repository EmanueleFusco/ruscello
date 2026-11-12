use ruscello_platform::language::Language;

pub(crate) struct Messages {
    pub usage: &'static str,
    pub failed: &'static str,
}

const EN: Messages = Messages {
    usage: "Usage: ruscello-cli get <url> [-o directory] [-x connections]",
    failed: "Ruscello could not complete the download.",
};

const IT: Messages = Messages {
    usage: "Uso: ruscello-cli get <url> [-o cartella] [-x connessioni]",
    failed: "Ruscello non ha potuto completare il download.",
};

pub(crate) fn get(language: Language) -> &'static Messages {
    match language {
        Language::English => &EN,
        Language::Italian => &IT,
    }
}
