<#
.SYNOPSIS
    CLI 变异测试：验证测试门禁真的能抓住代码 bug。

.EXAMPLE
    .\scripts\mutation-test.ps1              # 全量（慢，820+ 变异体）
    .\scripts\mutation-test.ps1 -Scope check  # 只变异 check.rs
    .\scripts\mutation-test.ps1 -Scope args   # 只变异 args.rs
    .\scripts\mutation-test.ps1 -List         # 只列出变异体，不跑

.NOTES
    -Jobs 2 降低并发，避开 Windows 上 cargo-mutants 的临时目录竞态
    （高并发时 worker 线程会报 "does not exist, refusing to create it"）。
    等价变异（如 bin main -> Default）标为 MISSED 属正常，不代表测试缺口。
#>
param(
    [ValidateSet('', 'check', 'args')]
    [string]$Scope = '',
    [switch]$List,
    [int]$Jobs = 2,
    [int]$Timeout = 120
)

$ErrorActionPreference = 'Stop'

$argsList = @('mutants', '-p', 'zephyra-cli', '-t', "$Timeout", '-j', "$Jobs")
if ($List) {
    $argsList += '--list'
}
switch ($Scope) {
    'check' { $argsList += @('--file', '**/commands/check.rs') }
    'args'  { $argsList += @('--file', '**/args.rs') }
}

Write-Host "Running: cargo $($argsList -join ' ')"
& cargo @argsList
exit $LASTEXITCODE
