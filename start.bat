@echo off
setlocal DisableDelayedExpansion
pushd "%~dp0" || exit /b 1
if not defined SEARCHCORD_DATA_DIR set "SEARCHCORD_DATA_DIR=%~dp0data"
set "SEARCHCORD_EXEC=%~dp0searchcord.exe"
if not exist "%SEARCHCORD_EXEC%" set "SEARCHCORD_EXEC=%~dp0target\release\searchcord.exe"
if exist ".searchcord-version" (
  set "SEARCHCORD_LAUNCHER_DIR=%~dp0"
  set "SEARCHCORD_EXEC="
  for /f "delims=" %%V in ('%SystemRoot%\System32\findstr.exe /r /x "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*" .searchcord-version') do set "SEARCHCORD_EXEC=%~dp0app\%%V\searchcord.exe"
)
if not exist "%SEARCHCORD_EXEC%" (
  echo Searchcord is missing. Extract the full Windows download before opening it.
  popd
  pause
  exit /b 1
)
"%SEARCHCORD_EXEC%" %*
set "SEARCHCORD_EXIT=%errorlevel%"
if errorlevel 1 (
  echo.
  echo Could not start. Check the error above and your archive path.
)
popd
pause
exit /b %SEARCHCORD_EXIT%
