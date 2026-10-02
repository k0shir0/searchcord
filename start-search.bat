@echo off
cd /d "%~dp0"
if exist ".venv\Scripts\python.exe" (
  .venv\Scripts\python.exe -u search_app.py %*
) else (
  python -u search_app.py %*
)
if errorlevel 1 (
  echo.
  echo Could not start. Check that Python 3.12+ is on PATH,
  echo install requirements.txt, and pass --data-dir with your archive directory.
)
pause
