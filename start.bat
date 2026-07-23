@echo off
cd /d "%~dp0"
echo Starting Searchcord on http://127.0.0.1:8000
python app.py
if errorlevel 1 (
  echo.
  echo Could not start. Check that Python 3.9+ is installed and on your PATH,
  echo then install dependencies with:
  echo     pip install -r requirements.txt
)
pause
