"""CI-only installer for a pinned Chrome for Testing browser and WebDriver."""
import os
from pathlib import Path
import platform
import tempfile
from urllib.request import urlopen
from zipfile import ZipFile

VERSION = '154.0.8037.57'
system = platform.system()
target = {'Linux': 'linux64', 'Windows': 'win64',
          'Darwin': 'mac-arm64' if platform.machine() == 'arm64' else 'mac-x64'}[system]
directory = Path(os.environ.get('RUNNER_TEMP', tempfile.gettempdir())) / 'searchcord-webdriver' / VERSION
directory.mkdir(parents=True, exist_ok=True)
for artifact in ('chrome', 'chromedriver'):
    archive = directory / (artifact + '.zip')
    with urlopen(f'https://storage.googleapis.com/chrome-for-testing-public/{VERSION}/{target}/{artifact}-{target}.zip', timeout=120) as response:
        with archive.open('wb') as output:
            import shutil
            shutil.copyfileobj(response, output)
    with ZipFile(archive) as package:
        package.extractall(directory)
    archive.unlink()
driver = directory / f'chromedriver-{target}' / ('chromedriver.exe' if system == 'Windows' else 'chromedriver')
browser = directory / f'chrome-{target}' / ({'Linux': 'chrome', 'Windows': 'chrome.exe',
    'Darwin': 'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing'}[system])
if system != 'Windows':
    # Zip extraction doesn't preserve executable bits, including helper apps.
    for path in directory.rglob('*'):
        if path.is_file() and (path == driver or path == browser or path.name in
                {'chrome_crashpad_handler', 'chrome_sandbox', 'chrome-sandbox'} or
                '.app/Contents/MacOS/' in str(path)):
            path.chmod(path.stat().st_mode | 0o755)
values = {'SEARCHCORD_WEBDRIVER': str(driver), 'SEARCHCORD_CHROME_BINARY': str(browser)}
if 'GITHUB_ENV' in os.environ:
    with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as output:
        for key, value in values.items():
            output.write(f'{key}={value}\n')
else:
    for key, value in values.items():
        print(f'{key}={value}')
