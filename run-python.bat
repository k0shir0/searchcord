@echo off
setlocal
rem Both launchers run here from the repository directory, including UNC paths.
if exist ".venv\Scripts\python.exe" goto dotvenv
if exist "venv\Scripts\python.exe" goto venv
py -3 -c "import sys; sys.exit(sys.version_info < (3, 12))" >nul 2>&1
if not errorlevel 1 goto py
python -c "import sys; sys.exit(sys.version_info < (3, 12))" >nul 2>&1
if not errorlevel 1 goto python
echo Python 3.12+ was not found. Install Python, then create .venv and install requirements.txt.
exit /b 1

:dotvenv
".venv\Scripts\python.exe" %*
exit /b %errorlevel%
:venv
"venv\Scripts\python.exe" %*
exit /b %errorlevel%
:py
py -3 %*
exit /b %errorlevel%
:python
python %*
exit /b %errorlevel%
