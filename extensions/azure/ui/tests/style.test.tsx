import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { PluginStyle } from '../src/PluginStyle';

const sdk = vi.hoisted(() => ({ resolve: vi.fn(), dispose: vi.fn() }));
const assets = { resolve: sdk.resolve };
vi.mock('@tabularis/plugin-api', () => ({ usePluginAssets: () => assets }));

async function settled() { await act(async () => { await Promise.resolve(); }); }
function stylesheet(container: HTMLElement) { return container.querySelector('link[rel="stylesheet"]') as HTMLLinkElement; }
function deferred() {
  let resolve!: (value: { url: string; dispose(): void }) => void;
  const promise = new Promise<{ url: string; dispose(): void }>(done => { resolve = done; });
  return { promise, resolve };
}
beforeEach(() => { sdk.dispose.mockReset(); sdk.resolve.mockReset().mockResolvedValue({ url: 'blob:owned-style', dispose: sdk.dispose }); });
afterEach(() => { cleanup(); vi.useRealTimers(); });

it('스타일 로드 성공은 명시한 플러그인 asset URL을 유지한다', async () => {
  // given
  const mounted = render(<PluginStyle pluginId="cosmos-nosql" />);
  const load = async () => { await settled(); fireEvent.load(stylesheet(mounted.container)); };
  // when
  await load();
  // then
  expect(sdk.resolve).toHaveBeenCalledExactlyOnceWith('ui/dist/style.css');
  expect(stylesheet(mounted.container)).toHaveAttribute('href', 'blob:owned-style');
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  expect(sdk.dispose).not.toHaveBeenCalled();
});

it('스타일을 소유한 컴포넌트가 닫히면 URL을 한 번 해제한다', async () => {
  // given
  const mounted = render(<PluginStyle pluginId="cosmos-nosql" />);
  await settled();
  // when
  mounted.unmount();
  // then
  expect(sdk.dispose).toHaveBeenCalledTimes(1);
  expect(mounted.container.querySelector('link')).toBeNull();
});

it('닫힌 뒤 늦게 받은 asset은 링크를 만들지 않고 즉시 해제한다', async () => {
  // given
  const pending = deferred();
  sdk.resolve.mockReturnValue(pending.promise);
  const mounted = render(<PluginStyle pluginId="cosmos-nosql" />);
  const closeThenResolve = async () => { mounted.unmount(); pending.resolve({ url: 'blob:late-style', dispose: sdk.dispose }); await settled(); };
  // when
  await closeThenResolve();
  // then
  expect(sdk.dispose).toHaveBeenCalledTimes(1);
  expect(mounted.container).toBeEmptyDOMElement();
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
});

it('CSS 로드 오류는 URL을 해제하고 짧은 오류 안내만 표시한다', async () => {
  // given
  const mounted = render(<PluginStyle pluginId="cosmos-nosql" />);
  await settled();
  // when
  fireEvent.error(stylesheet(mounted.container));
  // then
  expect(screen.getByRole('alert')).toHaveTextContent('Cosmos 스타일을 불러오지 못했습니다.');
  expect(mounted.container.querySelector('link')).toBeNull();
  expect(sdk.dispose).toHaveBeenCalledTimes(1);
});

it('asset 조회 실패는 원래 오류 내용을 화면에 노출하지 않는다', async () => {
  // given
  sdk.resolve.mockRejectedValue(new Error('synthetic-secret-canary'));
  const mounted = render(<PluginStyle pluginId="cosmos-nosql" />);
  // when
  await settled();
  // then
  expect(screen.getByRole('alert')).toHaveTextContent('Cosmos 스타일을 불러오지 못했습니다.');
  expect(mounted.container.textContent).not.toContain('synthetic-secret-canary');
  expect(sdk.dispose).not.toHaveBeenCalled();
});

it('10초 뒤에도 asset이 도착하지 않으면 실패하고 늦은 URL을 해제한다', async () => {
  // given
  vi.useFakeTimers();
  const pending = deferred();
  sdk.resolve.mockReturnValue(pending.promise);
  const mounted = render(<PluginStyle pluginId="cosmos-nosql" />);
  const expireThenResolve = async () => { await act(async () => { await vi.advanceTimersByTimeAsync(10_000); pending.resolve({ url: 'blob:expired-style', dispose: sdk.dispose }); }); };
  // when
  await expireThenResolve();
  // then
  expect(screen.getByRole('alert')).toHaveTextContent('Cosmos 스타일을 불러오지 못했습니다.');
  expect(mounted.container.querySelector('link')).toBeNull();
  expect(sdk.dispose).toHaveBeenCalledTimes(1);
  expect(vi.getTimerCount()).toBe(0);
});

it('별도 작업 공간은 여는 버튼의 스타일 수명과 독립적으로 유지된다', async () => {
  // given
  const firstDispose = vi.fn(), secondDispose = vi.fn();
  sdk.resolve.mockResolvedValueOnce({ url: 'blob:opener', dispose: firstDispose }).mockResolvedValueOnce({ url: 'blob:workspace', dispose: secondDispose });
  const opener = render(<PluginStyle pluginId="cosmos-nosql" />);
  const workspace = render(<PluginStyle pluginId="cosmos-nosql" />);
  await settled();
  // when
  opener.unmount();
  // then
  expect(firstDispose).toHaveBeenCalledTimes(1);
  expect(secondDispose).not.toHaveBeenCalled();
  expect(stylesheet(workspace.container)).toHaveAttribute('href', 'blob:workspace');
});
