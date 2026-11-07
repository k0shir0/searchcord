@echo off
setlocal DisableDelayedExpansion
set "SEARCHCORD_ROOT=%~dp0"
if not exist "%~dp0.searchcord-version" if not exist "%~dp0searchcord.exe" if exist "%~dp0..\..\Cargo.toml" (
  for %%I in ("%~dp0..\..") do set "SEARCHCORD_ROOT=%%~fI\"
)
pushd "%SEARCHCORD_ROOT%" || exit /b 1
if not defined SEARCHCORD_DATA_DIR set "SEARCHCORD_DATA_DIR=%SEARCHCORD_ROOT%data"
set "SEARCHCORD_EXEC=%SEARCHCORD_ROOT%searchcord.exe"
if not exist "%SEARCHCORD_EXEC%" set "SEARCHCORD_EXEC=%SEARCHCORD_ROOT%target\release\searchcord.exe"
if exist ".searchcord-version" (
  set "SEARCHCORD_LAUNCHER_DIR=%~dp0"
  set "SEARCHCORD_EXEC="
  for /f "delims=" %%V in ('%SystemRoot%\System32\findstr.exe /r /x "[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*" .searchcord-version') do set "SEARCHCORD_EXEC=%~dp0app\%%V\searchcord.exe"
)
if not exist "%SEARCHCORD_EXEC%" (
  echo Searchcord is missing. Use the full Windows download or build Searchcord first.
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
