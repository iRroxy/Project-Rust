$ErrorActionPreference = 'Stop'

$dockerCommand = Get-Command docker -ErrorAction SilentlyContinue
if (-not $dockerCommand) {
    $dockerBins = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\DockerDesktop\resources\bin'),
        (Join-Path $env:ProgramFiles 'Docker\Docker\resources\bin')
    )
    foreach ($dockerBin in $dockerBins) {
        if (Test-Path -LiteralPath (Join-Path $dockerBin 'docker.exe')) {
            $env:PATH = "$dockerBin;$env:PATH"
            $dockerCommand = Get-Command docker -ErrorAction SilentlyContinue
            break
        }
    }
}
if (-not $dockerCommand) {
    throw 'Docker CLI was not found. Install and start Docker Desktop, then run this script again.'
}

function Invoke-ProjectDocker {
    param([string[]]$DockerArguments)
    & $dockerCommand.Source @DockerArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Docker command failed (exit code $LASTEXITCODE). Check that Docker Desktop is running with Linux containers."
    }
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    Invoke-ProjectDocker -DockerArguments @('compose', 'version')
    Invoke-ProjectDocker -DockerArguments @('info', '--format', '{{.OSType}}')
    Invoke-ProjectDocker -DockerArguments @('compose', 'config', '--quiet')
    Invoke-ProjectDocker -DockerArguments @('compose', 'up', '-d', '--wait', 'postgres')
    Invoke-ProjectDocker -DockerArguments @('compose', 'ps')
    Invoke-ProjectDocker -DockerArguments @(
        'compose', 'exec', '-T', 'postgres',
        'psql', '-v', 'ON_ERROR_STOP=1', '-U', 'ai_saas', '-d', 'ai_saas',
        '-c', 'SELECT current_database(), current_user;',
        '-c', "SELECT name, default_version FROM pg_available_extensions WHERE name = 'vector';"
    )
    Write-Host 'PostgreSQL is ready. The vector extension will be enabled in a later migration.'
}
finally {
    Pop-Location
}
