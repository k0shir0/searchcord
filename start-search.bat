@echo off
setlocal
pushd "%~dp0" || exit /b 1
call "%~dp0run-python.bat" -u search_app.py %*
set "SEARCHCORD_EXIT=%errorlevel%"
if errorlevel 1 (
  echo.
  echo Could not start. Check that Python 3.12+ is installed,
  echo install requirements.txt, and pass --data-dir with your archive directory.
)
popd
pause
exit /b %SEARCHCORD_EXIT%
