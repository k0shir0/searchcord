@echo off
setlocal
pushd "%~dp0" || exit /b 1
if defined SEARCHCORD_PORT (set "SEARCHCORD_DISPLAY_PORT=%SEARCHCORD_PORT%") else (set "SEARCHCORD_DISPLAY_PORT=8000")
echo Preparing Searchcord on http://127.0.0.1:%SEARCHCORD_DISPLAY_PORT%
echo On a first database upgrade, this may take several minutes. The browser opens when ready.
call "%~dp0run-python.bat" -u app.py
set "SEARCHCORD_EXIT=%errorlevel%"
if errorlevel 1 (
  echo.
  echo Could not start. Check that Python 3.12+ is installed,
  echo then install dependencies with:
  echo     run-python.bat -m pip install -r requirements.txt
)
popd
pause
exit /b %SEARCHCORD_EXIT%
