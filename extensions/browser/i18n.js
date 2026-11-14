export const STRINGS = {
  en: {
    brand: "Ruscello",
    connected: "Connected & paired",
    unpaired: "Not paired",
    offline: "Ruscello app not running",
    pairingPending: "Approve pairing in Ruscello Settings…",
    pairOneClick: "One-click pair with Ruscello",
    pairPinPlaceholder: "Optional 6-digit PIN",
    pairSuccess: "Paired successfully!",
    pairFailed: "Pairing failed. Open Ruscello Settings → Integrations.",
    disconnect: "Disconnect",
    analyzeTab: "Analyze page with LinkGrabber",
    forwardCookies: "Send domain cookies to Ruscello",
    interceptToggle: "Intercept browser downloads",
    interceptDesc: "Send matching downloads directly to Ruscello",
    sendUrlPlaceholder: "Paste download or magnet URL…",
    sendUrlBtn: "Send",
    openOptions: "Filter & API settings",
    altSpeedBtn: "Alt speed",
    optionsTitle: "Ruscello Extension Settings",
    apiPortLabel: "Local API port",
    minSizeLabel: "Minimum intercepted file size (MiB)",
    mimeLabel: "Intercept MIME prefixes (comma-separated, blank = all)",
    languageLabel: "Language",
    saveBtn: "Save settings",
    savedMsg: "Settings saved.",
    actionOk: "Sent to Ruscello.",
    actionErr: "Request failed. Check connection.",
    candidatesFound: "{n} links found and analyzed.",
    cookiesSent: "{n} session cookies forwarded."
  },
  it: {
    brand: "Ruscello",
    connected: "Connesso e associato",
    unpaired: "Non associato",
    offline: "App Ruscello non in esecuzione",
    pairingPending: "Approva l'associazione nelle Impostazioni di Ruscello…",
    pairOneClick: "Associa con un clic a Ruscello",
    pairPinPlaceholder: "PIN a 6 cifre opzionale",
    pairSuccess: "Associazione completata!",
    pairFailed: "Associazione non riuscita. Apri Ruscello → Impostazioni → Integrazioni.",
    disconnect: "Disconnetti",
    analyzeTab: "Analizza pagina con LinkGrabber",
    forwardCookies: "Invia cookie di dominio a Ruscello",
    interceptToggle: "Intercetta download del browser",
    interceptDesc: "Invia i download compatibili direttamente a Ruscello",
    sendUrlPlaceholder: "Incolla URL di download o magnet…",
    sendUrlBtn: "Invia",
    openOptions: "Impostazioni filtri e API",
    altSpeedBtn: "Vel. alt.",
    optionsTitle: "Impostazioni estensione Ruscello",
    apiPortLabel: "Porta API locale",
    minSizeLabel: "Dimensione minima intercettazione (MiB)",
    mimeLabel: "Prefissi MIME da intercettare (separati da virgola, vuoto = tutti)",
    languageLabel: "Lingua",
    saveBtn: "Salva impostazioni",
    savedMsg: "Impostazioni salvate.",
    actionOk: "Inviato a Ruscello.",
    actionErr: "Richiesta non riuscita. Verifica la connessione.",
    candidatesFound: "{n} link trovati e analizzati.",
    cookiesSent: "{n} cookie di sessione inoltrati."
  }
};

export function resolveLang(pref) {
  if (pref === "en" || pref === "it") {
    return pref;
  }
  const ui = (globalThis.navigator && navigator.language) || "en";
  return ui.toLowerCase().startsWith("it") ? "it" : "en";
}

export function t(lang, key, vars) {
  const dict = STRINGS[lang] || STRINGS.en;
  let text = dict[key] || STRINGS.en[key] || key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      text = text.replace("{" + k + "}", String(v));
    }
  }
  return text;
}
