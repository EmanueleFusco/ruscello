import os
import time
import random
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC
from selenium.common.exceptions import NoSuchElementException, TimeoutException, WebDriverException

import undetected_chromedriver as uc


# --- Helper: Popup Handling and Re-Click ---
def handle_popups_and_reclick(driver, button_locator, max_attempts=2):
    """
    Check for popup windows after an action, close them, and re-click the target button.
    """
    for attempt in range(max_attempts):
        print(f"Checking for popups (Attempt {attempt + 1}/{max_attempts})...")
        time.sleep(random.uniform(2.5, 4.0))

        try:
            main_window = driver.current_window_handle
            windows = driver.window_handles

            if len(windows) > 1:
                print(f"Found {len(windows)} windows. Closing popups...")
                popup_closed = False
                for window in windows:
                    if window != main_window:
                        try:
                            driver.switch_to.window(window)
                            print(f"Closing popup window: {driver.title}")
                            driver.close()
                            popup_closed = True
                        except WebDriverException as e:
                            print(f"Minor error closing popup (may have closed itself): {e}")
                        except Exception as e:
                            print(f"Error closing popup window: {e}")

                try:
                    driver.switch_to.window(main_window)
                    if popup_closed:
                        print("Returned to main window.")
                    time.sleep(1)

                    print(f"Attempting to re-click button {button_locator}...")
                    wait = WebDriverWait(driver, 15)
                    button_to_reclick = wait.until(EC.element_to_be_clickable(button_locator))
                    button_to_reclick.click()
                    print("Button re-clicked successfully.")
                    return True
                except (NoSuchElementException, TimeoutException):
                    print(f"Failed to find or click button {button_locator} after closing popups.")
                    return False
                except Exception as e:
                    print(f"Unexpected error during re-click: {e}")
                    return False
            else:
                print("No popups detected.")
                return True

        except Exception as e:
            print(f"Error during popup handling: {e}")
            try:
                driver.switch_to.window(driver.window_handles[0])
            except Exception:
                pass
            return False

    print(f"Reached maximum attempts ({max_attempts}) for popup handling and re-click.")
    return False


# --- Helper: Verify Download Started ---
def verify_download_started(download_folder, files_before_click, timeout=30):
    """
    Poll the download folder for a new .crdownload or .part file.
    """
    print(f"Verifying download start (polling for new .crdownload/.part files for {timeout}s)...")
    start_time = time.time()
    while time.time() - start_time < timeout:
        try:
            files_now = set(os.listdir(download_folder))
            new_files = files_now - files_before_click
            for filename in new_files:
                if filename.endswith(".crdownload") or filename.endswith(".part"):
                    print(f"Download verified: found new file '{filename}'.")
                    return True
        except FileNotFoundError:
            print(f"Verification Error: Download folder '{download_folder}' not found.")
            return False
        except Exception as e:
            print(f"Error checking download folder during verification: {e}")
        time.sleep(2)

    print(f"Warning: Download start not verified within {timeout} seconds.")
    return False


# --- Main Download Function ---
def download_file(driver, link, download_folder):
    print(f"Navigating to: {link}")
    driver.get(link)
    print("Initial wait for page load and potential challenge...")
    time.sleep(random.uniform(12, 18))

    try:
        # --- 1. Find and click the first button ('method_free') ---
        print("Looking for first button 'method_free'...")
        method_free_button = WebDriverWait(driver, 30).until(
            EC.element_to_be_clickable((By.ID, "method_free"))
        )
        print("'method_free' found. Clicking.")
        method_free_button.click()
        print("Waiting after 'method_free' click...")
        time.sleep(random.uniform(4, 7))

        # --- 2. Handle popups after first click ---
        main_window_handle = driver.current_window_handle
        windows = driver.window_handles
        if len(windows) > 1:
            print(f"Found {len(windows)} windows after first click. Closing popups...")
            for window in windows:
                if window != main_window_handle:
                    try:
                        driver.switch_to.window(window)
                        print(f"Closing popup window: {driver.title}")
                        driver.close()
                    except Exception as e:
                        print(f"Error closing popup: {e}")
            try:
                driver.switch_to.window(main_window_handle)
                print("Returned to main window.")
                time.sleep(1)
            except Exception as e:
                print(f"Error returning to main window: {e}")

        # --- 3. Find the final button ('downloadbtn') ---
        print("Looking for final button 'downloadbtn'...")
        final_button_locator = (By.ID, "downloadbtn")
        download_button_final = WebDriverWait(driver, 25).until(
            EC.element_to_be_clickable(final_button_locator)
        )
        print("'downloadbtn' found.")

        # --- 4. Final click, popup handling/re-click, and download verification ---
        files_before_final_click = set()
        try:
            files_before_final_click = set(os.listdir(download_folder))
        except FileNotFoundError:
            print(f"Error: Download folder '{download_folder}' not found before final click.")

        print("Clicking 'downloadbtn'...")
        download_button_final.click()

        click_sequence_success = handle_popups_and_reclick(driver, final_button_locator, max_attempts=2)

        if click_sequence_success:
            print("Final click sequence completed (or no popups detected).")
            verify_download_started(download_folder, files_before_final_click, timeout=30)
        else:
            print("Failed to click 'downloadbtn' successfully even after popup handling.")

    except TimeoutException:
        print(f"Timeout Error: Required button ('method_free' or 'downloadbtn') was not clickable in time for {link}.")
    except NoSuchElementException:
        print(f"NoSuchElement Error: Could not find required button ('method_free' or 'downloadbtn') for {link}.")
    except Exception as e:
        print(f"Unexpected error processing link {link}: {e}")


# --- Check All Downloads Finished ---
def are_downloads_finished(download_folder):
    try:
        for filename in os.listdir(download_folder):
            if filename.endswith(".crdownload") or filename.endswith(".part"):
                return False
    except FileNotFoundError:
        print(f"Error: Download folder '{download_folder}' not found during final check.")
        return True
    except Exception as e:
        print(f"Error during final download check: {e}")
        return False
    return True


# --- Driver Setup ---
link_file = "links.txt"
download_folder = "downloads"

if not os.path.exists(download_folder):
    print(f"Download folder '{download_folder}' not found. Creating...")
    os.makedirs(download_folder)

options = uc.ChromeOptions()
options.add_argument("--disable-popup-blocking")
options.add_argument("--no-sandbox")
options.add_argument("--disable-dev-shm-usage")
options.add_argument("--window-size=1920,1080")
options.add_argument(
    "user-agent=Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0 Safari/537.36"
)

prefs = {
    "download.default_directory": download_folder,
    "download.prompt_for_download": False,
    "download.directory_upgrade": True,
    "safeBrowse.enabled": True,
}
options.add_experimental_option("prefs", prefs)

print("Starting Chrome driver with undetected-chromedriver...")
driver = None
try:
    driver = uc.Chrome(options=options, version_main=135)
    print("Driver started.")
except Exception as e:
    print(f"Error starting undetected-chromedriver: {e}")
    exit()

# --- Read Links ---
links = []
try:
    with open(link_file, "r") as f:
        links = [line.strip() for line in f if line.strip()]
    if not links:
        print(f"Warning: No valid links found in '{link_file}'.")
    else:
        print(f"Found {len(links)} links.")
except FileNotFoundError:
    print(f"Error: Link file '{link_file}' not found.")
    if driver:
        driver.quit()
    exit()

# --- Main Loop ---
if links and driver:
    for i, link in enumerate(links):
        print(f"\n--- Processing link {i+1}/{len(links)} ---")
        download_file(driver, link, download_folder)
        print(f"--- Finished processing link {i+1}/{len(links)} ---")
        pause = random.uniform(5, 10)
        print(f"Pausing for {pause:.1f} seconds...")
        time.sleep(pause)

# --- Wait for Completion ---
if links and driver:
    print("\nAll links processed. Waiting for downloads to finish...")
    wait_count = 0
    max_wait_cycles = 180
    while not are_downloads_finished(download_folder):
        if wait_count >= max_wait_cycles:
            print("Timeout: Maximum wait time for download completion reached.")
            break
        time.sleep(5)
        wait_count += 1
        if wait_count % 6 == 0:
            print(f"Still waiting for downloads to finish... ({wait_count*5}s)")
    print("\nDownloads completed (or timed out).")

if driver:
    print("Closing browser.")
    driver.quit()

print("Script finished.")
