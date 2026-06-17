@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
set PATH=%PATH%;C:\Users\herna\.cargo\bin
cd /d "D:\dev\side projects\clipX"

if not exist dist mkdir dist

echo === Building Server ===
cargo build --release -p clipx-server
if errorlevel 1 exit /b 1
copy /y target\release\clipx-server.exe dist\ >nul

echo === Building Client ===
cargo build --release -p clipx-client
if errorlevel 1 exit /b 1
copy /y target\release\clipx-client.exe dist\ >nul

echo === Building Core ===
dotnet build core\ClipX.Core.csproj -c Release
if errorlevel 1 exit /b 1

echo === All builds passed ===
echo Output: dist\
dir /b dist\*.exe
