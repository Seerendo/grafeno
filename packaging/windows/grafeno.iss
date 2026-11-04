; Grafeno for Windows: the installer (Inno Setup 6).
;
;   iscc /DAppVersion=0.1.0 packaging\windows\grafeno.iss
;
; It needs target\release\grafeno.exe built with Grafeno Studio inside
; (npm run build in studio\, then cargo build --release -p grafeno-server).
; The installer writes dist\grafeno-<version>-windows-x64-setup.exe.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define DataDir "{commonappdata}\Grafeno"
#define ConfigFile "{commonappdata}\Grafeno\grafeno.toml"

[Setup]
AppId={{5B0D1C8E-6C2A-4E7B-9F3A-0E8B6A1C4D27}
AppName=Grafeno
AppVersion={#AppVersion}
AppVerName=Grafeno {#AppVersion}
AppPublisher=Seerendo
AppPublisherURL=https://github.com/Seerendo/grafeno
DefaultDirName={autopf}\Grafeno
DefaultGroupName=Grafeno
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=grafeno-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
ChangesEnvironment=yes
WizardStyle=modern
SetupIconFile=grafeno.ico
UninstallDisplayIcon={app}\grafeno.exe
UninstallDisplayName=Grafeno {#AppVersion}
SetupLogging=yes

[Tasks]
Name: service; Description: "Run Grafeno as a Windows service (starts with Windows, port 7810)"
Name: path; Description: "Add grafeno to the system PATH (to use it from any terminal)"

[Files]
Source: "..\..\target\release\grafeno.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; DestName: "README.md"; Flags: ignoreversion
Source: "..\..\docs\query-language.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Dirs]
Name: "{#DataDir}"
Name: "{#DataDir}\data"

[INI]
Filename: "{group}\Grafeno Studio.url"; Section: "InternetShortcut"; Key: "URL"; String: "http://127.0.0.1:7810/studio/"; Tasks: service
Filename: "{group}\Grafeno Studio.url"; Section: "InternetShortcut"; Key: "IconFile"; String: "{app}\grafeno.exe"; Tasks: service
Filename: "{group}\Grafeno Studio.url"; Section: "InternetShortcut"; Key: "IconIndex"; String: "0"; Tasks: service

[Icons]
Name: "{group}\Grafeno Shell"; Filename: "{app}\grafeno.exe"; Parameters: "shell --config ""{#ConfigFile}"""; Comment: "GQ console connected to the local service"; Tasks: service
Name: "{group}\Grafeno settings"; Filename: "{sys}\notepad.exe"; Parameters: """{#ConfigFile}"""; Comment: "grafeno.toml: address, token, Studio"
Name: "{group}\Uninstall Grafeno"; Filename: "{uninstallexe}"

[Registry]
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: path; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
; Settings with a fresh token, only on the first install (upgrades keep them).
Filename: "{app}\grafeno.exe"; Parameters: "config init --path ""{#ConfigFile}"" --db ""{#DataDir}\data\grafeno.redb"""; Flags: runhidden waituntilterminated; StatusMsg: "Creating the settings and token..."; Check: not FileExists(ExpandConstant('{#ConfigFile}'))
Filename: "{app}\grafeno.exe"; Parameters: "service install --config ""{#ConfigFile}"""; Flags: runhidden waituntilterminated; StatusMsg: "Starting the Grafeno service..."; Tasks: service
Filename: "http://127.0.0.1:7810/studio/"; Description: "Open Grafeno Studio"; Flags: postinstall shellexec nowait skipifsilent; Tasks: service

[UninstallRun]
Filename: "{app}\grafeno.exe"; Parameters: "service uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveService"

[UninstallDelete]
Type: files; Name: "{group}\Grafeno Studio.url"

[Messages]
FinishedLabel=Grafeno is installed.%n%nGrafeno Studio: http://127.0.0.1:7810/studio/%nIts token is in C:\ProgramData\Grafeno\grafeno.toml (or run: grafeno config token).%n%nYour data stays in C:\ProgramData\Grafeno, also if you uninstall.

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE,
    'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

// Upgrades: stop and remove the old service so its binary can be replaced.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  OldExe: string;
begin
  Result := '';
  OldExe := ExpandConstant('{app}\grafeno.exe');
  if FileExists(OldExe) then
    Exec(OldExe, 'service uninstall', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

// Uninstall: take the install folder out of the system PATH again.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Dir: string;
  At: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE,
    'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path) then
    exit;
  Dir := ExpandConstant('{app}');
  At := Pos(';' + Uppercase(Dir), Uppercase(Path));
  if At > 0 then
  begin
    Delete(Path, At, Length(Dir) + 1);
    RegWriteExpandStringValue(HKEY_LOCAL_MACHINE,
      'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path);
  end;
end;
