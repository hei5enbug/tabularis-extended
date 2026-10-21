import { useEffect, useState } from 'react';
import { usePluginAssets } from '@tabularis/plugin-api';
import * as hostApi from '@tabularis/plugin-api';
import css from './styles.css?inline';

type StyleLink = { url: string; loaded(): void; failed(): void };

export function PluginStyle({ pluginId }: { pluginId: string }) {
  return typeof hostApi.usePluginAssets === 'function' ? <AssetStyle pluginId={pluginId} /> : <style>{css}</style>;
}

function AssetStyle({ pluginId }: { pluginId: string }) {
  const assets = usePluginAssets(pluginId);
  const [link, setLink] = useState<StyleLink | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    let expired = false;
    let dispose: (() => void) | undefined;
    const release = () => { const current = dispose; dispose = undefined; current?.(); };
    const failure = () => {
      if (!alive || expired) return;
      expired = true;
      clearTimeout(timer);
      release();
      setLink(null);
      setFailed(true);
    };
    const timer = setTimeout(failure, 10_000);
    setLink(null);
    setFailed(false);
    void assets.resolve('ui/dist/style.css').then(asset => {
      if (!alive || expired) { asset.dispose(); return; }
      dispose = asset.dispose;
      setLink({ url: asset.url, loaded: () => { if (alive && !expired) clearTimeout(timer); }, failed: failure });
    }).catch(failure);
    return () => { alive = false; clearTimeout(timer); release(); };
  }, [assets, pluginId]);
  return <>
    {link && <link rel="stylesheet" href={link.url} onLoad={link.loaded} onError={link.failed} />}
    {failed && <p role="alert">Cosmos 스타일을 불러오지 못했습니다.</p>}
  </>;
}
