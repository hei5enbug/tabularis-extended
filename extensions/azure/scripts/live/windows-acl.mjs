import path from 'node:path';
import { execFileSync } from 'node:child_process';

const powershellScript = String.raw`
$ErrorActionPreference = 'Stop'
try {
  $identitySid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $access = Get-Acl -LiteralPath $env:TABULARIS_LIVE_ACL_TARGET_PATH -ErrorAction Stop
  $binary = $access.GetSecurityDescriptorBinaryForm()
  $descriptor = [System.Security.AccessControl.RawSecurityDescriptor]::new($binary, 0)
  $daclPresent = ($descriptor.ControlFlags -band [System.Security.AccessControl.ControlFlags]::DiscretionaryAclPresent) -ne 0
  if (-not $daclPresent -or $null -eq $descriptor.DiscretionaryAcl) { exit 17 }
  $aces = [System.Collections.Generic.List[object]]::new()
  for ($index = 0; $index -lt $descriptor.DiscretionaryAcl.Count; $index++) {
    $ace = $descriptor.DiscretionaryAcl[$index]
    if ($ace.GetType().FullName -ne 'System.Security.AccessControl.CommonAce') { exit 17 }
    if ($ace.AceType -eq [System.Security.AccessControl.AceType]::AccessAllowed) {
      $kind = 'allow'
    } elseif ($ace.AceType -eq [System.Security.AccessControl.AceType]::AccessDenied) {
      $kind = 'deny'
    } else {
      exit 17
    }
    $aces.Add([ordered]@{ type = $kind; sid = $ace.SecurityIdentifier.Value })
  }
  $result = [ordered]@{
    currentSid = $identitySid
    ownerSid = $descriptor.Owner.Value
    daclPresent = $true
    aces = @($aces.ToArray())
  }
  [Console]::Out.WriteLine((ConvertTo-Json -InputObject $result -Compress -Depth 4))
} catch {
  exit 17
}
`;

const sidPattern = /^S-1-(?:\d+-)*\d+$/i;
const systemSid = 'S-1-5-18';
const administratorsSid = 'S-1-5-32-544';

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function hasKeys(value, keys) {
  return Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

export function ownerAclAllowed(value) {
  if (!isObject(value) || !hasKeys(value, ['currentSid', 'ownerSid', 'daclPresent', 'aces'])
    || typeof value.currentSid !== 'string' || !sidPattern.test(value.currentSid)
    || typeof value.ownerSid !== 'string' || !sidPattern.test(value.ownerSid) || value.daclPresent !== true
    || !Array.isArray(value.aces)) return false;
  const currentSid = value.currentSid.toUpperCase();
  if (value.ownerSid.toUpperCase() !== currentSid) return false;
  const allowedSids = new Set([currentSid, systemSid, administratorsSid]);
  return value.aces.every(ace => {
    if (!isObject(ace) || !hasKeys(ace, ['type', 'sid']) || typeof ace.sid !== 'string' || !sidPattern.test(ace.sid)) return false;
    if (ace.type === 'deny') return true;
    return ace.type === 'allow' && allowedSids.has(ace.sid.toUpperCase());
  });
}

export function assertOwnerAcl(target, { platform = process.platform, systemRoot = process.env.SystemRoot, exec = execFileSync } = {}) {
  if (platform !== 'win32') return;
  if (typeof target !== 'string' || target.length === 0 || typeof systemRoot !== 'string' || !path.win32.isAbsolute(systemRoot)) {
    throw new Error('OWNER_ACL_CHECK_FAILED');
  }
  const executable = path.win32.join(systemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
  let output;
  try {
    output = exec(executable, ['-NoProfile', '-NonInteractive', '-Command', powershellScript], {
      encoding: 'utf8',
      shell: false,
      windowsHide: true,
      timeout: 15000,
      maxBuffer: 64 * 1024,
      env: { SystemRoot: systemRoot, WINDIR: systemRoot, TABULARIS_LIVE_ACL_TARGET_PATH: target },
    });
  } catch {
    throw new Error('OWNER_ACL_CHECK_FAILED');
  }
  let result;
  try { result = JSON.parse(output); } catch { throw new Error('OWNER_ACL_CHECK_FAILED'); }
  if (!ownerAclAllowed(result)) throw new Error('OWNER_ACL_CHECK_FAILED');
}
