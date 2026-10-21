import { useId } from "react";
import type { Extra, Issue, IssueKey } from "./auth";
import { styles } from "./styles";

interface FieldProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  help?: string;
  error?: string;
  required?: boolean;
}

function TextField({ label, value, onChange, placeholder, help, error, required = false }: FieldProps) {
  const id = useId();
  const described = [help && `${id}-help`, error && `${id}-error`].filter(Boolean).join(" ") || undefined;
  return <div style={styles.field}>
    <label htmlFor={id} style={styles.label}>{label}</label>
    <input
      id={id}
      value={value}
      placeholder={placeholder}
      aria-required={required}
      aria-invalid={error ? true : undefined}
      aria-describedby={described}
      autoComplete="off"
      autoCapitalize="off"
      autoCorrect="off"
      spellCheck={false}
      onChange={(event) => onChange(event.target.value)}
      style={error ? { ...styles.control, ...styles.invalid } : styles.control}
    />
    {help && <p id={`${id}-help`} style={styles.help}>{help}</p>}
    {error && <p id={`${id}-error`} style={styles.error}>{error}</p>}
  </div>;
}

interface Props {
  extra: Extra;
  issues: readonly Issue[];
  setExtraField: (key: string, value: string) => void;
}

export default function AuthPanel({ extra, issues, setExtraField }: Props) {
  const message = (key: IssueKey) => issues.find((issue) => issue.key === key)?.message;

  return <fieldset style={styles.fieldset}>
    <legend style={styles.legend}>Microsoft Entra 사용자 (Azure CLI)</legend>
    <div style={styles.field}>
      <p style={styles.help}>
        로그인과 MFA는 앱 밖의 터미널에서 <code style={styles.code}>az login --tenant &lt;테넌트 ID&gt;</code>로
        진행합니다. 연결할 때마다 드라이버가 Azure CLI에서 토큰을 받아 오며, Azure CLI의 로그인 상태와 구독은 바꾸지 않습니다.
      </p>
      <p style={styles.help}>
        사용자 이름 입력란에는 az login에 쓴 Entra 계정(UPN)을 입력하세요. 암호는 쓰지 않으므로 입력란이 숨겨집니다.
      </p>
    </div>

    <div style={styles.columns}>
      <TextField
        label="테넌트 ID"
        required
        value={extra.tenant_id ?? ""}
        placeholder="00000000-0000-0000-0000-000000000000"
        error={message("tenant_id")}
        onChange={(value) => setExtraField("tenant_id", value)}
      />
      <TextField
        label="개체 ID"
        required
        value={extra.entra_object_id ?? ""}
        placeholder="00000000-0000-0000-0000-000000000000"
        help="az ad signed-in-user show --query id -o tsv 로 확인할 수 있습니다."
        error={message("entra_object_id")}
        onChange={(value) => setExtraField("entra_object_id", value)}
      />
    </div>
    <TextField
      label="Azure CLI 경로 (선택)"
      value={extra.azure_cli_path ?? ""}
      placeholder="비워 두면 PATH에서 az를 찾습니다"
      help="az 실행 파일의 절대 경로를 지정할 때만 입력하세요."
      error={message("azure_cli_path")}
      onChange={(value) => setExtraField("azure_cli_path", value)}
    />

    <div style={styles.field}>
      <p style={styles.label}>연결 조건</p>
      <ul style={styles.list}>
        <li>사용자 이름은 필수이며 Azure CLI에 로그인한 계정과 같아야 합니다.</li>
        <li>호스트는 <code style={styles.code}>.database.windows.net</code>으로 끝나는 Azure SQL 서버여야 합니다.</li>
        <li>SSL 모드는 <code style={styles.code}>verify-full</code>이어야 합니다.</li>
        <li>SQL Server Plus 플러그인 설정의 Trust Server Certificate는 꺼져 있어야 합니다.</li>
      </ul>
      <p style={styles.help}>이 조건은 이 입력란 밖의 값이라 연결할 때 드라이버가 확인합니다.</p>
    </div>
  </fieldset>;
}
