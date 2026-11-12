import os
import time
import random # Necessario per le pause randomizzate
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC
from selenium.common.exceptions import NoSuchElementException, TimeoutException, WebDriverException

# Importa undetected_chromedriver
import undetected_chromedriver as uc

# --- Funzione Helper: Gestione Popup e Re-Click ---
def handle_popups_and_reclick(driver, button_locator, max_attempts=2):
    """
    Controlla i popup dopo un'azione, li chiude e tenta di ricliccare il pulsante.

    Args:
        driver: L'istanza WebDriver di Selenium.
        button_locator: Il localizzatore (es. (By.ID, "downloadbtn")) del pulsante da ricliccare.
        max_attempts: Numero massimo di tentativi per cliccare il pulsante dopo aver chiuso i popup.

    Returns:
        True se il pulsante è stato cliccato con successo (o non sono apparsi popup), False altrimenti.
    """
    initial_click_assumed_done = True # Assume che il click iniziale sia stato fatto prima di chiamare questa funzione
    for attempt in range(max_attempts):
        print(f"Controllo popup (Tentativo {attempt + 1}/{max_attempts})...")
        time.sleep(random.uniform(2.5, 4.0)) # Attendi un po' che i popup appaiano

        try:
            main_window = driver.current_window_handle
            windows = driver.window_handles

            if len(windows) > 1:
                print(f"Trovate {len(windows)} finestre. Chiusura popup...")
                popup_closed = False
                for window in windows:
                    if window != main_window:
                        try:
                            driver.switch_to.window(window)
                            print(f"Chiusura finestra/popup: {driver.title}")
                            driver.close()
                            popup_closed = True
                        except WebDriverException as e:
                            print(f"Errore minore chiusura popup (potrebbe essersi chiuso da solo): {e}")
                            # Continua comunque a provare a tornare alla finestra principale
                        except Exception as e:
                             print(f"Errore chiusura finestra popup: {e}")

                # Assicurati di tornare alla finestra principale
                try:
                    driver.switch_to.window(main_window)
                    if popup_closed: print("Tornato alla finestra principale.")
                    time.sleep(1) # Pausa dopo essere tornati

                    # Tenta di cliccare di NUOVO il pulsante
                    print(f"Tento di ri-cliccare il pulsante {button_locator}...")
                    wait = WebDriverWait(driver, 15)
                    button_to_reclick = wait.until(EC.element_to_be_clickable(button_locator))
                    # driver.execute_script("arguments[0].scrollIntoView(true);", button_to_reclick) # Scroll opzionale
                    # time.sleep(0.5)
                    button_to_reclick.click()
                    print("Pulsante ri-cliccato con successo.")
                    # Dopo un re-click riuscito, usciamo dalla funzione helper assumendo che ora vada bene
                    return True
                except (NoSuchElementException, TimeoutException):
                    print(f"Fallito nel trovare o cliccare il pulsante {button_locator} dopo chiusura popup.")
                    return False # Fallito il re-click
                except Exception as e:
                    print(f"Errore inaspettato durante il re-click: {e}")
                    return False
            else:
                print("Nessun popup rilevato.")
                # Se non ci sono popup, il click iniziale (o un re-click precedente) dovrebbe essere andato a buon fine.
                return True # Successo (nessun popup da gestire)

        except Exception as e:
            print(f"Errore durante la gestione dei popup: {e}")
            # Prova a tornare alla finestra principale per sicurezza se possibile
            try:
                driver.switch_to.window(driver.window_handles[0])
            except: pass
            return False # Errore nella gestione popup

    print(f"Raggiunto numero massimo di tentativi ({max_attempts}) per gestire popup e re-click.")
    return False

# --- Funzione Helper: Verifica Inizio Download ---
def verify_download_started(download_folder, files_before_click, timeout=30):
    """
    Controlla la cartella di download per un nuovo file .crdownload o .part.

    Args:
        download_folder: Percorso della cartella di download.
        files_before_click: Un set di nomi file presenti prima del click di download.
        timeout: Secondi massimi di attesa per la verifica.

    Returns:
        True se viene rilevato un nuovo file di download, False altrimenti.
    """
    print(f"Verifica inizio download (controllo nuovi file .crdownload/.part per {timeout}s)...")
    start_time = time.time()
    while time.time() - start_time < timeout:
        try:
            files_now = set(os.listdir(download_folder))
            new_files = files_now - files_before_click
            for filename in new_files:
                if filename.endswith(".crdownload") or filename.endswith(".part"):
                    print(f"Download verificato: Trovato nuovo file '{filename}'.")
                    return True
        except FileNotFoundError:
            print(f"Errore Verifica: Cartella download '{download_folder}' non trovata.")
            return False # Non si può verificare se la cartella non esiste
        except Exception as e:
            print(f"Errore controllo cartella download durante verifica: {e}")
            # Non ritorna False subito, potrebbe essere un problema temporaneo
        time.sleep(2) # Controlla ogni 2 secondi

    print(f"Attenzione: Inizio download non verificato entro {timeout} secondi.")
    return False

# --- Funzione Principale di Download ---
def download_file(driver, link, download_folder): # Aggiunto download_folder come argomento
    print(f"Navigando verso: {link}")
    driver.get(link)
    print("Attesa iniziale per caricamento pagina e potenziale sfida Cloudflare...")
    time.sleep(random.uniform(12, 18)) # Attesa iniziale randomizzata

    try:
        # --- 1. Trova e clicca il primo pulsante ('method_free') ---
        print("Tentativo di trovare il primo pulsante 'method_free'...")
        method_free_button = WebDriverWait(driver, 30).until(
            EC.element_to_be_clickable((By.ID, "method_free"))
        )
        print("'method_free' trovato. Click.")
        method_free_button.click()
        print("Attesa dopo click su 'method_free'...")
        time.sleep(random.uniform(4, 7))

        # --- 2. Gestione Pop-up DOPO primo click (semplificata) ---
        main_window_handle = driver.current_window_handle
        windows = driver.window_handles
        if len(windows) > 1:
            print(f"Trovate {len(windows)} finestre dopo primo click. Chiusura popup...")
            for window in windows:
                if window != main_window_handle:
                    try:
                        driver.switch_to.window(window)
                        print(f"Chiusura finestra/popup: {driver.title}")
                        driver.close()
                    except Exception as e: print(f"Errore chiusura popup: {e}")
            try: # Assicurati di tornare alla finestra principale
                driver.switch_to.window(main_window_handle)
                print("Tornato alla finestra principale.")
                time.sleep(1)
            except Exception as e:
                 print(f"Errore nel tornare alla finestra principale: {e}")
                 # Potrebbe essere problematico continuare se non si è sulla finestra giusta

        # --- 3. Trova il pulsante finale ('downloadbtn') ---
        print("Tentativo di trovare il pulsante finale 'downloadbtn'...")
        final_button_locator = (By.ID, "downloadbtn")
        download_button_final = WebDriverWait(driver, 25).until(
            EC.element_to_be_clickable(final_button_locator)
        )
        print("'downloadbtn' trovato.")

        # --- 4. Click finale, gestione popup/re-click, verifica download ---
        files_before_final_click = set()
        try:
             # Ottieni lo stato della cartella PRIMA del click finale
             files_before_final_click = set(os.listdir(download_folder))
        except FileNotFoundError:
             print(f"Errore: Cartella download '{download_folder}' non trovata prima del click finale.")
             # Decide come gestire: uscire? continuare senza verifica? Continuiamo ma logghiamo.
             # La verifica fallirà o darà errore.

        print("Click su 'downloadbtn'...")
        download_button_final.click() # Primo tentativo di click finale

        # Usa la funzione helper per gestire popup e potenziali re-click
        click_sequence_success = handle_popups_and_reclick(driver, final_button_locator, max_attempts=2)

        if click_sequence_success:
            print("Sequenza click finale completata (o nessun popup rilevato).")
            # Verifica se il download è partito
            verify_download_started(download_folder, files_before_final_click, timeout=30)
        else:
            print("Fallito nel cliccare 'downloadbtn' con successo anche dopo gestione popup.")
            # Il download probabilmente non è partito.

    except TimeoutException as e:
        print(f"Timeout Errore: Un pulsante richiesto ('method_free' o 'downloadbtn') non è stato trovato/cliccabile nel tempo limite per {link}.")
    except NoSuchElementException as e:
        print(f"NoSuchElement Errore: Impossibile trovare un pulsante richiesto ('method_free' o 'downloadbtn') per {link}.")
    except Exception as e:
        print(f"Errore inaspettato durante il processamento del link {link}: {e}")


# --- Funzione Verifica Fine Download (invariata) ---
def are_downloads_finished(download_folder):
    try:
        for filename in os.listdir(download_folder):
            if filename.endswith(".crdownload") or filename.endswith(".part"):
                return False
    except FileNotFoundError:
        print(f"Errore: Cartella download '{download_folder}' non trovata durante verifica finale.")
        return True
    except Exception as e:
        print(f"Errore durante verifica finale download: {e}")
        return False # Per sicurezza, assumi non finito
    return True

# --- Configurazione e Setup Driver (invariato, usa undetected-chromedriver) ---
link_file = "C:\\Users\\emanu\\Desktop\\links.txt"
download_folder = "C:\\Users\\emanu\\Desktop\\download"

if not os.path.exists(download_folder):
    print(f"Cartella download '{download_folder}' non trovata. Creazione...")
    os.makedirs(download_folder)

options = uc.ChromeOptions()
options.add_argument("--disable-popup-blocking") # Potrebbe non essere efficace contro tutti i tipi di popup
options.add_argument('--no-sandbox')
options.add_argument('--disable-dev-shm-usage')
options.add_argument("--window-size=1920,1080")
options.add_argument(f"user-agent=Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 Safari/537.36") # Aggiorna se necessario

prefs = {
    "download.default_directory": download_folder,
    "download.prompt_for_download": False,
    "download.directory_upgrade": True,
    "safeBrowse.enabled": True # Mantenere attivo per sicurezza
}
options.add_experimental_option("prefs", prefs)
# options.add_argument('--headless=new') # Prova la modalità headless se vuoi meno finestre, ma può fallire più facilmente con Cloudflare/popup

print("Avvio del driver Chrome con undetected-chromedriver...")
driver = None
try:
    # Ricorda di specificare version_main se uc.Chrome() non trova la tua versione automaticamente
    driver = uc.Chrome(options=options, version_main=135)
    print("Driver avviato.")
except Exception as e:
    print(f"Errore avvio undetected-chromedriver: {e}")
    print("Verifica compatibilità ChromeDriver/Chrome.")
    exit()

# --- Lettura Link (invariato) ---
links = []
try:
    with open(link_file, "r") as f:
        links = [line.strip() for line in f if line.strip()]
    if not links:
        print(f"Attenzione: Nessun link valido trovato in '{link_file}'.")
    else:
        print(f"Trovati {len(links)} link.")
except FileNotFoundError:
    print(f"Errore: File link '{link_file}' non trovato.")
    if driver: driver.quit()
    exit()

# --- Ciclo Principale di Download ---
if links and driver:
    for i, link in enumerate(links):
        print(f"\n--- Processando link {i+1}/{len(links)} ---")
        # Passa la cartella di download alla funzione
        download_file(driver, link, download_folder)
        print(f"--- Fine processamento link {i+1}/{len(links)} ---")
        # Pausa randomizzata tra i link
        pause = random.uniform(5, 10)
        print(f"Pausa per {pause:.1f} secondi...")
        time.sleep(pause)

# --- Attesa Completamento Download (invariato) ---
if links and driver:
    print("\nTutti i link processati. Attesa completamento download...")
    wait_count = 0
    max_wait_cycles = 180 # Max 15 minuti (180 * 5s)
    while not are_downloads_finished(download_folder):
        if wait_count >= max_wait_cycles:
            print("Timeout: Attesa massima per completamento download raggiunta.")
            break
        time.sleep(5)
        wait_count += 1
        if wait_count % 6 == 0: # Messaggio ogni 30s
             print(f"Ancora in attesa completamento download... ({wait_count*5}s)")
    print("\nDownload completati (o timeout/errori).")

# --- Chiusura Driver ---
if driver:
    print("Chiusura del browser.")
    driver.quit()

print("Script terminato.")