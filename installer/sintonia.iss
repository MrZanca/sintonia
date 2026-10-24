; Installer di Sintonia per Windows (x64), con Inno Setup 6.
; Si compila con scripts\installer.ps1 (o in GitHub Actions), che passa versione e cartella dei file:
;   ISCC /DVersion=0.4.0 /DSourceDir=..\dist\Sintonia-0.4.0-windows-x64 installer\sintonia.iss
; L'installer non tocca i dati dell'utente (%APPDATA%\Sintonia): disinstallando restano.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef SourceDir
  #error "manca /DSourceDir: la cartella con sintonia.exe e libmpv-2.dll"
#endif

[Setup]
; identificativo fisso: serve agli aggiornamenti per riconoscere l'installazione precedente
AppId={{6F3B2C1E-8D4A-4E7B-9A51-5C0D2E7F1A93}
AppName=Sintonia
AppVersion={#Version}
AppVerName=Sintonia {#Version}
AppPublisher=MrZanca
AppPublisherURL=https://github.com/MrZanca/sintonia
AppSupportURL=https://github.com/MrZanca/sintonia/issues
DefaultDirName={autopf}\Sintonia
DefaultGroupName=Sintonia
DisableProgramGroupPage=yes
; installazione per l'utente o per tutti, a scelta (senza diritti di amministratore si può sempre)
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=Sintonia-{#Version}-windows-x64-setup
SetupIconFile=..\assets\logo\sintonia.ico
UninstallDisplayIcon={app}\sintonia.exe
UninstallDisplayName=Sintonia
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Sintonia aperta: l'installer chiede di chiuderla prima di sostituire i file
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "it"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
it.NoVulkan=Sul PC manca vulkan-1.dll, che arriva con i driver della scheda video (NVIDIA, AMD o Intel).%n%nSenza, il motore video di Sintonia non parte. Installa o aggiorna i driver video, poi apri Sintonia.%n%nContinuare comunque?
en.NoVulkan=vulkan-1.dll is missing on this PC. It comes with the graphics card drivers (NVIDIA, AMD or Intel).%n%nWithout it Sintonia's video engine won't start. Install or update your graphics drivers, then open Sintonia.%n%nContinue anyway?

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\sintonia.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Sintonia"; Filename: "{app}\sintonia.exe"
Name: "{autodesktop}\Sintonia"; Filename: "{app}\sintonia.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\sintonia.exe"; Description: "{cm:LaunchProgram,Sintonia}"; Flags: nowait postinstall skipifsilent

[Code]
{ libmpv-2.dll dipende da vulkan-1.dll (driver video). Su PC senza (alcune macchine virtuali)
  il video non parte: meglio dirlo subito che lasciare una finestra nera. }
function InitializeSetup(): Boolean;
begin
  Result := True;
  if not FileExists(ExpandConstant('{sys}\vulkan-1.dll')) then
    Result := (SuppressibleMsgBox(CustomMessage('NoVulkan'), mbConfirmation, MB_YESNO, IDYES) = IDYES);
end;
