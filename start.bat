@echo off
cd /d "%~dp0"
if defined SEARCHCORD_PORT (set "SEARCHCORD_DISPLAY_PORT=%SEARCHCORD_PORT%") else (set "SEARCHCORD_DISPLAY_PORT=8000")
echo Preparing Searchcord on http://127.0.0.1:%SEARCHCORD_DISPLAY_PORT%
echo On a first database upgrade, this may take several minutes. The browser opens when ready.
if exist ".venv\Scripts\python.exe" (
  .venv\Scripts\python.exe -u app.py
) else (
  python -u app.py
)
if errorlevel 1 (
  echo.
  echo Could not start. Check that Python 3.12+ is installed and on your PATH,
  echo then install dependencies with:
  echo     pip install -r requirements.txt
)
pause
