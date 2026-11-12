import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { validateCatalog, searchPlugins, installLink, downloadChoices } from '../../docs/plugins/catalog.mjs';

const document = JSON.parse(await readFile(new URL('../../docs/plugins.json', import.meta.url)));
const catalog = document.plugins;

test('released catalog validates and generates an encoded desktop install link', () => {
  assert.equal(validateCatalog(document), catalog);
  const link = new URL(installLink(catalog[0].download[0]));
  assert.equal(link.protocol, 'hydra:');
  assert.equal(link.hostname, 'install-plugin');
  assert.equal(link.searchParams.get('url'), catalog[0].download[0].link);
});

test('name search ignores case and surrounding spaces and can return no results', () => {
  assert.deepEqual(searchPlugins(catalog, '  YOUTUBE  '), [catalog[0]]);
  assert.deepEqual(searchPlugins(catalog, ''), catalog);
  assert.deepEqual(searchPlugins(catalog, 'unknown'), []);
  assert.deepEqual(searchPlugins(catalog, 'Hydra Team'), []);
  assert.deepEqual(searchPlugins([], ''), []);
});

test('malformed catalogs, duplicate identities and unsafe links are rejected', () => {
  for (const invalid of [null, {}, { ...document, plugins: null }, { ...document, schema: 2 }, { ...document, name: "" }, { ...document, plugins: [null] }, { ...document, plugins: [catalog[0], catalog[0]] }]) {
    assert.throws(() => validateCatalog(invalid));
  }
  for (const change of [
    { sha256: 'bad' }, { publisher_key: '' }, { api: 0 }, { name: '' }, { author: 42 }, { is_official: 'true' }, { id: '../escape' },
    { download: 'http://example.com/a.hyaplugin' },
    { download: 'https://example.com/a.exe' },
    { homepage: 'javascript:alert(1)' },
    { homepage: 'https://user:pass@example.com/' },
    { image: 'plugins/img/hydra.youtube/../other.svg' },
    { image: 'https://example.com/image.svg' },
  ]) {
    assert.throws(() => validateCatalog({ ...document, plugins: [{ ...catalog[0], ...change }] }));
  }
  assert.deepEqual(validateCatalog({ ...document, plugins: [] }), []);
});

test('community entries can omit checksum and signing key', () => {
  const plugin = { ...catalog[0], is_official: false };
  delete plugin.sha256;
  delete plugin.publisher_key;
  assert.deepEqual(validateCatalog({ ...document, plugins: [plugin] }), [plugin]);
});


test('each torrent platform has its own download, checksum and install link', () => {
  const torrent = catalog.find(plugin => plugin.id === 'hydra.torrent');
  assert.equal(torrent.download.length, 6);
  assert.equal(new Set(torrent.download.map(choice => choice.platform)).size, 6);
  for (const choice of torrent.download) {
    assert.equal(new URL(installLink(choice)).searchParams.get('url'), choice.link);
    // Packages released before plugins.yml keep the unversioned name until their next release.
    const versioned = `torrent-download_${torrent.version}_${choice.platform.replace('-', '_')}`;
    assert.match(choice.link, new RegExp(`/(${versioned}|torrent-download-${choice.platform})\\.hyaplugin$`));
    assert.match(choice.sha256, /^[a-f0-9]{64}$/);
  }
});

test('legacy single downloads remain supported', () => {
  const plugin = { ...catalog[0], download: catalog[0].download[0].link };
  assert.equal(validateCatalog({ ...document, plugins: [plugin] })[0], plugin);
  assert.deepEqual(downloadChoices(plugin), [{ caption: 'All platforms', link: plugin.download }]);
});

test('empty and malformed download choices are rejected', () => {
  for (const download of [[], null, 42, [null], [{ caption: '', link: 'https://example.com/p.hyaplugin' }],
    [{ caption: 'Linux', link: 'http://example.com/p.hyaplugin' }],
    [{ caption: 'Linux', link: 'https://example.com/p.exe' }],
    [{ caption: 'Linux', link: 'https://example.com/p.hyaplugin#fragment' }],
    [{ caption: 'Linux', link: 'https://example.com/p.hyaplugin', sha256: 'bad' }],
    [{ caption: 'Linux', link: 'https://example.com/p.hyaplugin', platform: '' }]]) {
    assert.throws(() => validateCatalog({ ...document, plugins: [{ ...catalog[0], download }] }));
  }
});
