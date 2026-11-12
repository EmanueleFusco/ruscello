import os
import sys
import time
from seleniumbase import Driver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC
from selenium.common.exceptions import TimeoutException, ElementClickInterceptedException


# Indicatori di una pagina di challenge Cloudflare a pagina intera
# (NON semplici widget Turnstile embedded nella pagina normale)
CLOUDFLARE_FULLPAGE_INDICATORS = [
    "verifica di sicurezza",
    "security check",
    "checking your browser",
    "stiamo verificando",
    "just a moment",
    "esecuzione della verifica",
]


DOWNLOAD_XPATH = "//*[self::a or self::button][contains(normalize-space(.), 'DOWNLOAD')]"


def is_cloudflare_challenge(driver):
    """
    Verifica se siamo su una pagina di challenge Cloudflare a pagina intera.
    Ritorna True SOLO se il bottone DOWNLOAD non esiste E ci sono indicatori Cloudflare.
    """
    try:
        # Se il bottone DOWNLOAD esiste, NON siamo su un challenge Cloudflare
        buttons = driver.find_elements(By.XPATH, DOWNLOAD_XPATH)
        if buttons:
            return False

        # Il bottone non c'e' - verifica se e' effettivamente Cloudflare
        page_source = driver.page_source.lower()
        return any(ind in page_source for ind in CLOUDFLARE_FULLPAGE_INDICATORS)
    except Exception:
        return False


def solve_cloudflare(driver, max_wait=20):
    """
    Tenta di risolvere il challenge Cloudflare Turnstile.
    Ritorna True se superato, False altrimenti.
    """
    print("[!] Challenge Cloudflare a pagina intera rilevato! Tento di risolverlo...")

    try:
        driver.uc_gui_click_captcha()
        print("[OK] Click sul captcha eseguito. Attendo la risoluzione...")
    except Exception as e:
        print(f"[!] uc_gui_click_captcha fallito ({e}), provo metodo alternativo...")
        try:
            driver.uc_gui_handle_captcha()
            print("[OK] Captcha gestito con metodo alternativo.")
        except Exception as e2:
            print(f"[!] Anche il metodo alternativo fallito: {e2}")

    # Attendi che il challenge venga superato (cerca il bottone DOWNLOAD)
    for i in range(max_wait):
        time.sleep(1)
        try:
            buttons = driver.find_elements(By.XPATH, DOWNLOAD_XPATH)
            if buttons:
                print("[OK] Challenge Cloudflare superato! Pagina di download caricata.")
                return True
        except Exception:
            pass
        if i % 5 == 4:
            print(f"   Ancora in attesa... ({i+1}s)")

    print("[X] Timeout: il challenge Cloudflare non e' stato superato.")
    return False


def enable_auto_download(driver, download_folder):
    """
    Imposta (o ri-imposta) il download automatico via CDP.
    Necessario dopo uc_open_with_reconnect / uc_gui_click_captcha perche'
    la disconnessione temporanea di ChromeDriver resetta la sessione CDP.
    """
    try:
        driver.execute_cdp_cmd("Browser.setDownloadBehavior", {
            "behavior": "allow",
            "downloadPath": download_folder,
            "eventsEnabled": True,
        })
    except Exception:
        pass
    try:
        driver.execute_cdp_cmd("Page.setDownloadBehavior", {
            "behavior": "allow",
            "downloadPath": download_folder,
        })
    except Exception:
        pass


# Funzione per gestire i download di ogni link (versione migliorata)
def download_file(driver, link, download_folder, max_retries=2):
    url = link.strip()
    print(f"Navigando verso: {url}")

    for attempt in range(max_retries + 1):
        # Naviga verso il link
        try:
            driver.uc_open_with_reconnect(url, reconnect_time=4)
        except Exception:
            driver.get(url)

        time.sleep(2)  # Attendi il caricamento della pagina

        # Controlla se c'e' un challenge Cloudflare a pagina intera
        if is_cloudflare_challenge(driver):
            if attempt < max_retries:
                solved = solve_cloudflare(driver)
                if solved:
                    break  # Challenge superato, procedi con il download
                else:
                    print(f"[!] Tentativo {attempt + 1}/{max_retries + 1} fallito. Riprovo...")
                    continue
            else:
                print(f"[X] Impossibile superare Cloudflare per {url} dopo {max_retries + 1} tentativi.")
                return
        else:
            break  # Nessun challenge, procedi con il download

    try:
        # Ri-applica la cartella di download dopo il reconnect di UC Mode
        enable_auto_download(driver, download_folder)

        # Salva l'handle della finestra principale prima di qualsiasi click
        main_window_handle = driver.current_window_handle

        # Trova il pulsante di download
        wait = WebDriverWait(driver, 20)
        download_button = wait.until(
            EC.element_to_be_clickable((By.XPATH, DOWNLOAD_XPATH))
        )

        # --- PRIMO CLICK (spesso apre un popup o invia richiesta HTMX) ---
        print("Eseguo il primo click...")
        try:
            download_button.click()
        except Exception:
            driver.execute_script("arguments[0].click();", download_button)
        time.sleep(3) # Pausa per dare tempo al popup o alla richiesta HTMX di avviarsi

        # --- STRATEGIA CHIAVE: Chiudiamo tutti i popup e clicchiamo una seconda volta se necessario ---
        current_handles = driver.window_handles
        if len(current_handles) > 1:
            print(f"Popup/Nuove schede rilevate ({len(current_handles) - 1}). Le chiudo...")
            for handle in current_handles:
                if handle != main_window_handle:
                    driver.switch_to.window(handle)
                    driver.close()
            # Assicurati di tornare alla finestra principale
            driver.switch_to.window(main_window_handle)
            enable_auto_download(driver, download_folder)
            print("Tornato alla finestra principale.")

            # --- SECONDO CLICK (se il primo click ha solo aperto un popup di ad) ---
            print("Eseguo il secondo click per avviare il download...")
            try:
                download_button_again = wait.until(
                    EC.element_to_be_clickable((By.XPATH, DOWNLOAD_XPATH))
                )
                try:
                    download_button_again.click()
                except Exception:
                    driver.execute_script("arguments[0].click();", download_button_again)
            except Exception:
                pass
        
        print(f"Download per {url} avviato correttamente.")
        time.sleep(5) # Attendi che il processo di download inizi

    except TimeoutException:
        print(f"ERRORE: Pulsante di download non trovato o non cliccabile per {url}")
    except ElementClickInterceptedException:
        print(f"ERRORE: Il click e' stato intercettato da un altro elemento per {url}")
    except Exception as e:
        print(f"ERRORE inaspettato per {url}: {e}")


# Funzione per verificare se ci sono ancora download in corso
def are_downloads_finished(download_folder):
    for filename in os.listdir(download_folder):
        if filename.endswith(".crdownload") or filename.endswith(".part"):
            return False
    return True

# --- SCRIPT PRINCIPALE ---

# Assicurati che la cartella di download esista
download_folder = "C:\\Users\\emanu\\Desktop\\download" 
if not os.path.exists(download_folder):
    os.makedirs(download_folder)

# Percorso del file con i link
link_file = "C:\\Users\\emanu\\Desktop\\links.txt"

# Inizializza il Driver con SeleniumBase UC Mode (Undetected Chromedriver)
# uc=True: attiva la modalità anti-rilevamento (crea già un profilo temporaneo pulito)
# NOTA: incognito=True non va usato perché in Chrome forza la finestra "Salva con nome"
print("Avvio del browser in modalita' anti-rilevamento (UC Mode)...")
driver = Driver(
    uc=True,
    chromium_arg="--disable-popup-blocking",
)

# Imposta la cartella di download tramite CDP (Chrome DevTools Protocol)
enable_auto_download(driver, download_folder)
print(f"Cartella di download impostata: {download_folder}")

# Leggi i link dal file di testo
try:
    with open(link_file, "r") as f:
        links = f.readlines()
except FileNotFoundError:
    print(f"ERRORE: Il file {link_file} non è stato trovato.")
    links = []

# Itera attraverso ogni link
for link in links:
    if link.strip(): # Salta righe vuote
        download_file(driver, link, download_folder)
        print("-" * 30)

# Attendi che i download terminino
print("Tutti i download sono stati avviati. Attendo il loro completamento...")
while not are_downloads_finished(download_folder):
    print("Download ancora in corso...")
    time.sleep(10) # Controlla ogni 10 secondi

print("Tutti i download sono completati. Chiusura del browser.")
driver.quit()