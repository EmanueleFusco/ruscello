import os
import sys
import time
from seleniumbase import Driver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC
from selenium.common.exceptions import TimeoutException, ElementClickInterceptedException


# Indicators of a full-page Cloudflare challenge
# (NOT simple Turnstile widgets embedded in a regular page)
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
    Check whether we are on a full-page Cloudflare challenge.
    Returns True ONLY if the DOWNLOAD button does not exist AND Cloudflare indicators are present.
    """
    try:
        # If the DOWNLOAD button exists, we are NOT on a Cloudflare challenge page
        buttons = driver.find_elements(By.XPATH, DOWNLOAD_XPATH)
        if buttons:
            return False

        # Button not found - check whether it is actually Cloudflare
        page_source = driver.page_source.lower()
        return any(ind in page_source for ind in CLOUDFLARE_FULLPAGE_INDICATORS)
    except Exception:
        return False


def solve_cloudflare(driver, max_wait=20):
    """
    Legacy script helper for full-page Cloudflare Turnstile challenge.
    (Note: in Ruscello, challenges are handled via human-in-the-loop session reuse — see ADR-0004.)
    """
    print("[!] Full-page Cloudflare challenge detected! Attempting resolution...")

    try:
        driver.uc_gui_click_captcha()
        print("[OK] Captcha click executed. Waiting for resolution...")
    except Exception as e:
        print(f"[!] uc_gui_click_captcha failed ({e}), trying fallback method...")
        try:
            driver.uc_gui_handle_captcha()
            print("[OK] Captcha handled via fallback method.")
        except Exception as e2:
            print(f"[!] Fallback method also failed: {e2}")

    # Wait for the challenge to clear (look for the DOWNLOAD button)
    for i in range(max_wait):
        time.sleep(1)
        try:
            buttons = driver.find_elements(By.XPATH, DOWNLOAD_XPATH)
            if buttons:
                print("[OK] Cloudflare challenge passed! Download page loaded.")
                return True
        except Exception:
            pass
        if i % 5 == 4:
            print(f"   Still waiting... ({i+1}s)")

    print("[X] Timeout: Cloudflare challenge was not passed.")
    return False


def enable_auto_download(driver, download_folder):
    """
    Configure (or re-configure) automatic downloads via CDP.
    Needed after uc_open_with_reconnect / uc_gui_click_captcha because
    temporary ChromeDriver disconnection resets the CDP session.
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


# Handle downloading a single link
def download_file(driver, link, download_folder, max_retries=2):
    url = link.strip()
    print(f"Navigating to: {url}")

    for attempt in range(max_retries + 1):
        try:
            driver.uc_open_with_reconnect(url, reconnect_time=4)
        except Exception:
            driver.get(url)

        time.sleep(2)  # Wait for page load

        # Check for full-page Cloudflare challenge
        if is_cloudflare_challenge(driver):
            if attempt < max_retries:
                solved = solve_cloudflare(driver)
                if solved:
                    break
                else:
                    print(f"[!] Attempt {attempt + 1}/{max_retries + 1} failed. Retrying...")
                    continue
            else:
                print(f"[X] Could not pass Cloudflare for {url} after {max_retries + 1} attempts.")
                return
        else:
            break

    try:
        # Re-apply download directory after UC Mode reconnect
        enable_auto_download(driver, download_folder)

        # Save main window handle before clicking
        main_window_handle = driver.current_window_handle

        # Find the download button
        wait = WebDriverWait(driver, 20)
        download_button = wait.until(
            EC.element_to_be_clickable((By.XPATH, DOWNLOAD_XPATH))
        )

        # --- FIRST CLICK (often opens an ad popup or triggers an HTMX request) ---
        print("Performing first click...")
        try:
            download_button.click()
        except Exception:
            driver.execute_script("arguments[0].click();", download_button)
        time.sleep(3)

        # --- KEY STRATEGY: Close all popups and click a second time if needed ---
        current_handles = driver.window_handles
        if len(current_handles) > 1:
            print(f"Popups/new tabs detected ({len(current_handles) - 1}). Closing them...")
            for handle in current_handles:
                if handle != main_window_handle:
                    driver.switch_to.window(handle)
                    driver.close()
            driver.switch_to.window(main_window_handle)
            enable_auto_download(driver, download_folder)
            print("Returned to main window.")

            # --- SECOND CLICK (if the first click only opened an ad popup) ---
            print("Performing second click to start download...")
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

        print(f"Download for {url} started.")
        time.sleep(5)

    except TimeoutException:
        print(f"ERROR: Download button not found or not clickable for {url}")
    except ElementClickInterceptedException:
        print(f"ERROR: Click was intercepted by another element for {url}")
    except Exception as e:
        print(f"Unexpected ERROR for {url}: {e}")


# Check whether any browser downloads are still in progress
def are_downloads_finished(download_folder):
    for filename in os.listdir(download_folder):
        if filename.endswith(".crdownload") or filename.endswith(".part"):
            return False
    return True


# --- MAIN SCRIPT ---

download_folder = "C:\\Users\\emanu\\Desktop\\download"
if not os.path.exists(download_folder):
    os.makedirs(download_folder)

link_file = "C:\\Users\\emanu\\Desktop\\links.txt"

print("Starting browser in UC Mode...")
driver = Driver(
    uc=True,
    chromium_arg="--disable-popup-blocking",
)

enable_auto_download(driver, download_folder)
print(f"Download folder configured: {download_folder}")

try:
    with open(link_file, "r") as f:
        links = f.readlines()
except FileNotFoundError:
    print(f"ERROR: File {link_file} not found.")
    links = []

for link in links:
    if link.strip():
        download_file(driver, link, download_folder)
        print("-" * 30)

print("All downloads triggered. Waiting for completion...")
while not are_downloads_finished(download_folder):
    print("Downloads still in progress...")
    time.sleep(10)

print("All downloads completed. Closing browser.")
driver.quit()
