# Read-only evidence collection. Never installs a service, elevates, changes UAC,
# locks a session, enters credentials or sends input. See the adjacent runbook.
[CmdletBinding()]
param(
    [ValidateSet('HostInventory', 'Baseline', 'Installed', 'Disabled', 'Uninstalled')]
    [string]$Stage = 'HostInventory',
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$BundleZip
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne 'Win32NT') {
    throw 'Windows is required.'
}
if (-not [Environment]::Is64BitProcess) {
    throw 'Run in 64-bit PowerShell to inspect the x64 installation.'
}

function Read-Probe([scriptblock]$Action) {
    try { [ordered]@{ status = 'collected'; value = @(& $Action) } }
    catch { [ordered]@{ status = 'unavailable'; error = $_.Exception.Message } }
}

$machine = Get-CimInstance Win32_ComputerSystem
$isVm = ($machine.Model -match 'Virtual Machine|VMware|VirtualBox|KVM|QEMU|HVM domU|Parallels|BHYVE')
if ($Stage -ne 'HostInventory' -and -not $isVm) {
    throw "Guest acceptance refuses an unrecognized/physical machine ($($machine.Manufacturer), $($machine.Model)). No operation was performed."
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
$installRoot = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'pb-mapper-desktop'
$report = [ordered]@{
    schema = 1
    collectedAtUtc = [DateTime]::UtcNow.ToString('o')
    stage = $Stage
    acceptance = 'not_established_by_this_collector'
    machine = [ordered]@{
        manufacturer = $machine.Manufacturer
        model = $machine.Model
        recognizedVirtualMachine = $isVm
        os = Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber
    }
    account = [ordered]@{
        elevatedAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        accountType = 'record_standard_or_administrator_in_observations'
        processSessionId = (Get-Process -Id $PID).SessionId
    }
    uac = Read-Probe {
        $policy = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
        foreach ($name in 'EnableLUA', 'PromptOnSecureDesktop', 'ConsentPromptBehaviorAdmin', 'ConsentPromptBehaviorUser') {
            $property = $policy.PSObject.Properties[$name]
            [pscustomobject]@{ name = $name; value = $(if ($property) { $property.Value } else { $null }) }
        }
    }
    service = Read-Probe {
        Get-CimInstance Win32_Service -Filter "Name = 'PbMapperDesktop'" |
            Select-Object Name, State, StartMode, StartName, PathName, ProcessId
    }
    desktopProcesses = Read-Probe {
        Get-CimInstance Win32_Process -Filter "Name = 'pb_mapper_ui.exe' OR Name = 'pb-mapper-desktop-helper.exe'" |
            Select-Object Name, ProcessId, ParentProcessId, SessionId, ExecutablePath
    }
}

if ($BundleZip) {
    $report.bundle = Read-Probe {
        $archivePath = (Resolve-Path -LiteralPath $BundleZip).Path
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
        try {
            $entries = @($archive.Entries.FullName | ForEach-Object { $_.Replace('\', '/') })
            $uiEntries = @($entries | Where-Object { $_ -match '(^|/)pb_mapper_ui\.exe$' })
            $required = 'pb_mapper_ui.exe', 'pb_mapper_ffi.dll', 'pb-mapper-desktop-helper.exe', 'flutter_windows.dll', 'data/app.so'
            $missing = @($required)
            if ($uiEntries.Count -eq 1) {
                $prefix = $uiEntries[0].Substring(0, $uiEntries[0].Length - 'pb_mapper_ui.exe'.Length)
                $missing = @($required | Where-Object { ($prefix + $_) -notin $entries })
            }
            [pscustomobject]@{
                fileName = [IO.Path]::GetFileName($archivePath)
                sha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
                entryCount = $entries.Count
                uiEntryCount = $uiEntries.Count
                missingRequiredFiles = $missing
                requiredFilesPresent = ($uiEntries.Count -eq 1 -and $missing.Count -eq 0)
            }
        } finally { $archive.Dispose() }
    }
}

if ($Stage -eq 'HostInventory') {
    $report.virtualization = [ordered]@{
        commands = Read-Probe {
            foreach ($name in 'Get-VM', 'VBoxManage', 'vmrun', 'qemu-system-x86_64', 'WindowsSandbox', 'vmconnect') {
                $command = Get-Command $name -ErrorAction SilentlyContinue
                [pscustomobject]@{ name = $name; available = [bool]$command; source = $(if ($command) { $command.Source } else { $null }) }
            }
        }
        hyperV = Read-Probe { Get-VM -ErrorAction Stop | Select-Object Name, State, Generation }
        hyperVCim = Read-Probe {
            Get-CimInstance -Namespace root/virtualization/v2 -ClassName Msvm_ComputerSystem |
                Where-Object ElementName -ne $machine.Name | Select-Object ElementName, EnabledState
        }
        sandboxPackage = Read-Probe { Get-AppxPackage '*Sandbox*' | Select-Object Name, Version }
    }
} else {
    $report.installation = Read-Probe {
        $configPath = Join-Path $installRoot 'installation.json'
        $paths = @($installRoot)
        $configPresent = Test-Path -LiteralPath $configPath -PathType Leaf
        $ownerMatchesCurrentUser = $null
        if ($configPresent) {
            $configInfo = Get-Item -LiteralPath $configPath
            if ($configInfo.Length -gt 8192 -or ($configInfo.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw 'Refusing an oversized or reparse-point installation configuration.'
            }
            $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
            $ownerMatchesCurrentUser = $config.owner_sid -eq $identity.User.Value
            foreach ($path in @($configPath, $config.ui_executable, $config.helper_executable)) {
                $fullPath = [IO.Path]::GetFullPath($path)
                if (-not $fullPath.StartsWith($installRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
                    throw 'Installation configuration points outside the expected installation root.'
                }
                $paths += $fullPath
            }
        }
        [pscustomobject]@{
            configurationPresent = $configPresent
            ownerMatchesCurrentUser = $ownerMatchesCurrentUser
            objects = @($paths | ForEach-Object {
                $path = $_
                if (Test-Path -LiteralPath $path) {
                    $acl = Get-Acl -LiteralPath $path
                    $item = Get-Item -LiteralPath $path
                    [pscustomobject]@{
                        path = $path
                        attributes = $item.Attributes.ToString()
                        sddl = $acl.Sddl
                        sha256 = $(if (-not $item.PSIsContainer) { (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash } else { $null })
                    }
                }
            })
        }
    }
}

$outputPath = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $outputPath -Force | Out-Null
$name = '{0}-{1}-{2}.json' -f [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffZ'), $Stage.ToLowerInvariant(), ([Guid]::NewGuid().ToString('N').Substring(0, 8))
$destination = Join-Path $outputPath $name
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $destination -Encoding UTF8
Write-Output $destination
