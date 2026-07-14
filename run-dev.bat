@echo off
REM Launches esbuild watch and the Python dev server in two separate windows.
start "GoldenEye - esbuild watch" cmd /k "npm run dev"
start "GoldenEye - dev server" cmd /k "python dev-server.py 8765"
echo.
echo Both terminals launched. Open http://localhost:8765
