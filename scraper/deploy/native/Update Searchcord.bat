@echo off
setlocal DisableDelayedExpansion
pushd "%~dp0" || exit /b 1
set "SEARCHCORD_UPDATE="
for /f "delims=" %%V in ('%SystemRoot%\System32\findstr.exe /r /x "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*" .searchcord-version') do set "SEARCHCORD_UPDATE=%~dp0app\%%V\update.ps1"
if not exist "%SEARCHCORD_UPDATE%" (
  echo Searchcord is missing. Extract the full Windows download before updating it.
  popd
  pause
  exit /b 1
)
rem The downloaded ZIP can mark its scripts as internet files. This policy is process-local.
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%SEARCHCORD_UPDATE%"
set "SEARCHCORD_EXIT=%errorlevel%"
if errorlevel 1 echo Update did not complete.
popd
pause
exit /b %SEARCHCORD_EXIT%
