; Grafeno for Windows: the installer of the engine (Inno Setup 6).
;
;   iscc /DAppVersion=0.1.0 packaging\windows\grafeno.iss
;
; It needs target\release\grafeno.exe (cargo build --release -p
; grafeno-server). The engine alone: Grafeno Studio is installed apart.
; The installer writes dist\grafeno-<version>-windows-x64-setup.exe.
;
; A guided setup, as PostgreSQL's is: the program folder, the data folder,
; the port, whether other computers may connect (with a firewall rule), the
; service and the PATH. Unattended, the same answers are parameters:
;
;   grafeno-<version>-windows-x64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES
;     /DIR="C:\Grafeno" /DATADIR="D:\GrafenoData" /PORT=7810 /NETWORK=0
;     /TASKS="service,path"
;
; Upgrades (running a newer setup over an older one) keep the settings and
; the data; uninstalling keeps them too (C:\ProgramData\Grafeno and the data
; folder).

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define SettingsDir "{commonappdata}\Grafeno"
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
CloseApplications=no

[Tasks]
Name: service; Description: "Run Grafeno as a Windows service (it starts with Windows)"
Name: path; Description: "Add grafeno to the system PATH (to use it from any terminal)"

[Files]
Source: "..\..\target\release\grafeno.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; DestName: "README.md"; Flags: ignoreversion
Source: "..\..\docs\install.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "..\..\docs\query-language.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Dirs]
Name: "{#SettingsDir}"
Name: "{code:DataDir}"; Flags: uninsneveruninstall

[Icons]
Name: "{group}\Grafeno Shell"; Filename: "{app}\grafeno.exe"; Parameters: "shell --config ""{#ConfigFile}"""; Comment: "GQ console connected to the local service"; Tasks: service
Name: "{group}\Grafeno settings"; Filename: "{sys}\notepad.exe"; Parameters: """{#ConfigFile}"""; Comment: "grafeno.toml: address, token, limits"
Name: "{group}\Uninstall Grafeno"; Filename: "{uninstallexe}"

[Registry]
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: path; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
; Settings with a fresh token, only on the first install (upgrades keep them).
Filename: "{app}\grafeno.exe"; Parameters: "config init --path ""{#ConfigFile}"" --db ""{code:DataDir}\grafeno.redb"" --addr {code:Address}"; Flags: runhidden waituntilterminated; StatusMsg: "Creating the settings and token..."; Check: not FileExists(ExpandConstant('{#ConfigFile}'))
; Other computers may connect: a firewall rule for the port.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Grafeno {code:Port}"" dir=in action=allow protocol=TCP localport={code:Port}"; Flags: runhidden waituntilterminated; StatusMsg: "Opening the port in the firewall..."; Check: OpensFirewall
Filename: "{app}\grafeno.exe"; Parameters: "service install --config ""{#ConfigFile}"""; Flags: runhidden waituntilterminated; StatusMsg: "Starting the Grafeno service..."; Tasks: service

[UninstallRun]
Filename: "{app}\grafeno.exe"; Parameters: "service uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveService"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Grafeno {code:InstalledPort}"""; Flags: runhidden waituntilterminated; RunOnceId: "RemoveFirewallRule"

[Messages]
FinishedLabel=Grafeno is installed.%n%nThe engine is a service: connect to it with its console (Grafeno Shell), its HTTP API, or Grafeno Studio (installed apart). Its token: grafeno config token.%n%nThe settings (C:\ProgramData\Grafeno) and the data stay if you uninstall.

[Code]
var
  ServerPage: TInputQueryWizardPage;
  NetworkPage: TInputOptionWizardPage;
  DataPage: TInputDirWizardPage;

// What the settings file of an earlier install says, if there is one.
function Upgrading(): Boolean;
begin
  Result := FileExists(ExpandConstant('{#ConfigFile}'));
end;

function DefaultDataDir(): String;
begin
  Result := ExpandConstant('{#SettingsDir}\data');
end;

procedure InitializeWizard();
begin
  DataPage := CreateInputDirPage(wpSelectDir, 'Data folder',
    'Where should Grafeno keep its databases?',
    'The service keeps its database here. It stays when Grafeno is uninstalled or upgraded.',
    False, '');
  DataPage.Add('');
  DataPage.Values[0] := ExpandConstant('{param:DATADIR|' + DefaultDataDir() + '}');

  ServerPage := CreateInputQueryPage(DataPage.ID, 'Server',
    'Which port should the engine listen on?',
    'Clients (the console, applications, Grafeno Studio) connect to this port.');
  ServerPage.Add('Port:', False);
  ServerPage.Values[0] := ExpandConstant('{param:PORT|7810}');

  NetworkPage := CreateInputOptionPage(ServerPage.ID, 'Network',
    'Who may connect to the engine?',
    'Listening on the network also adds a firewall rule for the port. Every request needs the token (or an account) either way.',
    True, False);
  NetworkPage.Add('Only this computer (127.0.0.1)');
  NetworkPage.Add('Other computers too (every network interface, firewall rule)');
  if ExpandConstant('{param:NETWORK|0}') = '1' then
    NetworkPage.SelectedValueIndex := 1
  else
    NetworkPage.SelectedValueIndex := 0;
end;

// An upgrade keeps its settings: the questions about them are skipped.
function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := Upgrading() and
    ((PageID = DataPage.ID) or (PageID = ServerPage.ID) or (PageID = NetworkPage.ID));
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Value: Integer;
begin
  Result := True;
  if CurPageID = ServerPage.ID then
  begin
    Value := StrToIntDef(Trim(ServerPage.Values[0]), 0);
    if (Value < 1) or (Value > 65535) then
    begin
      MsgBox('The port is a number from 1 to 65535.', mbError, MB_OK);
      Result := False;
    end;
  end;
end;

function DataDir(Param: String): String;
begin
  Result := Trim(DataPage.Values[0]);
  if Result = '' then
    Result := DefaultDataDir();
end;

function Port(Param: String): String;
begin
  Result := IntToStr(StrToIntDef(Trim(ServerPage.Values[0]), 7810));
end;

function Address(Param: String): String;
begin
  if NetworkPage.SelectedValueIndex = 1 then
    Result := '0.0.0.0:' + Port('')
  else
    Result := '127.0.0.1:' + Port('');
end;

function OpensFirewall(): Boolean;
begin
  Result := (not Upgrading()) and (NetworkPage.SelectedValueIndex = 1);
end;

// The port in the installed settings (for the firewall rule's name).
function InstalledPort(Param: String): String;
var
  Lines: TArrayOfString;
  I, J: Integer;
  Line: String;
begin
  Result := '7810';
  if not LoadStringsFromFile(ExpandConstant('{#ConfigFile}'), Lines) then
    exit;
  for I := 0 to GetArrayLength(Lines) - 1 do
  begin
    Line := Trim(Lines[I]);
    if Pos('addr', Line) = 1 then
      for J := Length(Line) downto 1 do
        if Line[J] = ':' then
        begin
          // addr = "127.0.0.1:7810": what follows the last colon, without
          // the closing quote.
          Result := Copy(Line, J + 1, Length(Line) - J - 1);
          break;
        end;
  end;
end;

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
