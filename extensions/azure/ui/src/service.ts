import type { Operation, OperationInputs, ServiceRequest, ServiceResponse } from '@tabularis/service-contracts/types';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import { UiError } from './models';

export function request<O extends Operation>(operation: O, connectionId: string | null, input: OperationInputs[O], expectedVersion: number | null = null): ServiceRequest<O> {
  return { protocol_version: 1, operation, request_id: crypto.randomUUID(), connection_id: connectionId, session_id: null, map_id: null, expected_version: expectedVersion, deadline_ms: 60_000, input };
}
export function completed(response: ServiceResponse): ServiceResponse {
  if (response.status === 'outcome_unknown' || response.error?.outcome === 'unknown') throw new UiError('OUTCOME_UNKNOWN', response);
  if (response.status !== 'succeeded' || response.error) throw new UiError(response.error?.code || (response.status === 'cancelled' || response.status === 'interrupted' ? 'CANCELLED' : 'REQUEST_FAILED'), response);
  return response;
}
export class RequestScope {
  private epoch = 0;
  private controllers = new Set<AbortController>();
  constructor(private service: UsePluginServiceReturn) {}
  get generation(): number { return this.epoch; }
  cancel(): void { this.epoch += 1; this.controllers.forEach(controller => controller.abort()); this.controllers.clear(); }
  async run<T>(action: (signal: AbortSignal) => Promise<T>): Promise<T> {
    const epoch = this.epoch;
    const controller = new AbortController();
    this.controllers.add(controller);
    try {
      const result = await action(controller.signal);
      if (epoch !== this.epoch || controller.signal.aborted) throw new UiError('STALE_RESPONSE');
      return result;
    } catch (error) {
      if (epoch !== this.epoch || controller.signal.aborted) throw new UiError('STALE_RESPONSE');
      if (error instanceof UiError) throw error;
      throw new UiError('REQUEST_FAILED');
    } finally { this.controllers.delete(controller); }
  }
  async call<O extends Operation>(operation: O, connectionId: string | null, input: OperationInputs[O], expectedVersion: number | null = null): Promise<ServiceResponse> {
    return this.run(async signal => completed(await this.service.call(request(operation, connectionId, input, expectedVersion), { signal })));
  }
}
