@echo off
setlocal
pushd "%~dp0" || exit /b 1
if not defined SEARCHCORD_DATA_DIR set "SEARCHCORD_DATA_DIR=%~dp0data"
set "SEARCHCORD_EXEC=%~dp0searchcord-search.exe"
if not exist "%SEARCHCORD_EXEC%" set "SEARCHCORD_EXEC=%~dp0target\release\searchcord-search.exe"
if not exist "%SEARCHCORD_EXEC%" (
  echo Searchcord Search is missing. Build it with cargo build --release --locked --workspace.
  popd
  pause
  exit /b 1
)
"%SEARCHCORD_EXEC%" %*
set "SEARCHCORD_EXIT=%errorlevel%"
if errorlevel 1 (
  echo.
  echo Could not start. Check the error above and pass --data-dir with your archive directory.
)
popd
pause
exit /b %SEARCHCORD_EXIT%
