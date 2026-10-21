import type { PartitionKeyComponent } from '@tabularis/service-contracts/types';
import { UiError } from './models';

export interface KeyField { type: PartitionKeyComponent['type']; value: string }
export function keyComponents(fields: KeyField[]): PartitionKeyComponent[] {
  return fields.map((field): PartitionKeyComponent => {
    if (field.type === 'null') return { type: 'null' };
    if (field.type === 'undefined') return { type: 'undefined' };
    if (field.type === 'string') return { type: 'string', value: field.value };
    if (field.type === 'boolean') return { type: 'boolean', value: field.value === 'true' };
    const value = Number(field.value);
    if (!field.value.trim() || !Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) throw new UiError('INVALID_PARTITION_KEY');
    return { type: 'number', value };
  });
}
export function PartitionKeyFields({ paths, fields, onChange, label = '파티션 키', partial = false }: { paths: string[]; fields: KeyField[]; onChange: (fields: KeyField[]) => void; label?: string; partial?: boolean }) {
  const change = (index: number, field: KeyField) => onChange(fields.map((item, position) => position === index ? field : item));
  return <fieldset><legend>{label}</legend>{paths.map((path, index) => fields[index] && <div className="cosmos-key" key={`${index}:${path}`}>
    <label>{`${path} 형식`}<select value={fields[index].type} onChange={event => change(index, { type: event.target.value as KeyField['type'], value: event.target.value === 'boolean' ? 'true' : '' })}>{['string', 'number', 'boolean', 'null', 'undefined'].map(type => <option key={type} value={type}>{type === 'undefined' ? 'missing (속성 없음)' : type}</option>)}</select></label>
    {(fields[index].type === 'string' || fields[index].type === 'number') && <label>{`${path} 값`}<input value={fields[index].value} onChange={event => change(index, { ...fields[index], value: event.target.value })} /></label>}
    {fields[index].type === 'boolean' && <label>{`${path} 값`}<select value={fields[index].value} onChange={event => change(index, { ...fields[index], value: event.target.value })}><option value="true">true</option><option value="false">false</option></select></label>}
  </div>)}{partial && <p>앞에서부터 입력한 구성 요소만 쿼리 범위에 사용할 수 있습니다.</p>}</fieldset>;
}
