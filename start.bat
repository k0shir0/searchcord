@echo off
setlocal
pushd "%~dp0" || exit /b 1
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
