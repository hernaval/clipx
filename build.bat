@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
set PATH=%PATH%;C:\Users\herna\.cargo\bin
cd /d "D:\dev\side projects\clipX"

echo === Building Server ===
cargo check -p clipx-server
if errorlevel 1 exit /b 1

echo === Building Client ===
cargo check -p clipx-client
if errorlevel 1 exit /b 1

echo === Building Core ===
dotnet build core\ClipX.Core.csproj
if errorlevel 1 exit /b 1

echo === All builds passed ===
