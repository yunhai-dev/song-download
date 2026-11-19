const { createServer } = require('node:http');
const { timingSafeEqual } = require('node:crypto');
const { dirname, join } = require('node:path');

// Never inherit credential storage settings from the parent environment. The only QQ settings this
// process uses are written inside startup from the JSON configuration line and removed immediately
// after the package has read them.
delete process.env.QQ_AUTH_SESSION_PATH;
delete process.env.QQ_SESSION_SECRET;
delete process.env.QQ_SESSION_SECRET_PREVIOUS;
// Embed QQ's Koa callback without letting the package open its own listener.
process.env.NODE_ENV = 'test';
process.env.LOG_LEVEL = 'error';

let qq = null;
let netease = null;
let qqBackend = null;
let qqFileStore = null;

const qqRoutes = new Set([
  '/login/qr/key', '/login/qr/create', '/login/qr/check', '/login/status', '/logout',
  '/search',
  '/user/playlist', '/user/playlist-detail', '/getSongListDetail', '/getMusicPlay',
  '/user/liked-songs', '/getLyric',
]);
const neteaseRoutes = new Map([
  ['/login/qr/key', 'login_qr_key'],
  ['/login/qr/create', 'login_qr_create'],
  ['/login/qr/check', 'login_qr_check'],
  ['/login/status', 'login_status'],
  ['/user/account', 'user_account'],
  ['/search', 'search'],
  ['/user/playlist', 'user_playlist'],
  ['/playlist/track/all', 'playlist_track_all'],
  ['/song/url/v1', 'song_url_v1'],
  ['/lyric/new', 'lyric_new'],
]);

function fail(code) {
  // Fixed marker only: never echo the startup line, a secret, or a filesystem path.
  console.log(`SONG_DOWNLOAD_ERROR=${code}`);
  process.exit(1);
}

function parseStartupConfig(line) {
  const value = JSON.parse(line);
  if (!value || typeof value !== 'object' || !('qq' in value)) throw new Error('Invalid startup configuration');
  if (value.qq === null) return null;
  const config = value.qq;
  if (!config || typeof config !== 'object' ||
      typeof config.path !== 'string' || config.path.length === 0 ||
      typeof config.secret !== 'string' || config.secret.length === 0) {
    throw new Error('Invalid startup configuration');
  }
  return { path: config.path, secret: config.secret };
}

function readStartupConfig() {
  return new Promise((resolve, reject) => {
    let buffer = '';
    let settled = false;
    const settle = (callback, value) => {
      if (settled) return;
      settled = true;
      callback(value);
    };
    const onData = (chunk) => {
      // Stay attached and keep discarding after the first line so the stream keeps flowing and can
      // still emit `end`; pausing it here would make a later EOF invisible.
      if (settled) return;
      buffer += chunk;
      const newline = buffer.indexOf('\n');
      if (newline === -1) {
        if (buffer.length > 65536) settle(reject, new Error('Startup configuration too large'));
        return;
      }
      if (buffer.length > newline + 1) {
        settle(reject, new Error('Unexpected startup data'));
        return;
      }
      try {
        settle(resolve, parseStartupConfig(buffer.slice(0, newline)));
      } catch (error) {
        settle(reject, error);
      }
    };
    const onError = (error) => settle(reject, error);
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', onData);
    process.stdin.on('error', onError);
    process.stdin.resume();
  });
}

function loadProviders() {
  if (qq) return;
  qq = require('@yakult-green-tea/qq-music-api');
  netease = require('@neteasecloudmusicapienhanced/api');
}

function qqPackageRoot() {
  return dirname(require.resolve('@yakult-green-tea/qq-music-api/package.json'));
}

// The package root does not export its persistence factory, so resolve the pinned runtime file.
function loadQqBackend() {
  if (!qqBackend) {
    qqBackend = require(join(qqPackageRoot(), 'dist', 'src', 'services', 'auth', 'fileAuthSessionRepository.js'));
  }
  if (typeof qqBackend.createConfiguredAuthSessionRepository !== 'function') {
    throw new Error('Unsupported QQ persistence backend');
  }
  return qqBackend;
}

function qqRouteMethods() {
  const { routeManifest } = require(join(qqPackageRoot(), 'dist', 'src', 'routes', 'manifest.js'));
  const methods = new Map();
  for (const route of routeManifest) {
    const segments = route.path.split('/');
    while (segments.length > 1 && segments[segments.length - 1].startsWith(':')) segments.pop();
    methods.set(segments.join('/') || '/', route.method);
  }
  return methods;
}

function memoryQqRepository() {
  return { kind: 'memory', load: () => [], save: () => {} };
}

function memoryQqStore() {
  qq.configureAuthSessionRepository(memoryQqRepository());
}

// One explicit load proves the file and secret are usable; the package otherwise swallows a failed
// load and silently continues with an empty in-memory store.
function configureQqStore(config) {
  if (!config) {
    memoryQqStore();
    return;
  }
  qqFileStore = config;
  try {
    const backend = loadQqBackend();
    const repository = backend.createConfiguredAuthSessionRepository({
      QQ_AUTH_SESSION_PATH: config.path,
      QQ_SESSION_SECRET: config.secret,
    });
    if (!repository) throw new Error('QQ persistence is not configured');
    const loaded = repository.load();
    if (loaded !== null && loaded !== undefined && !Array.isArray(loaded)) {
      throw new Error('QQ persistence state is invalid');
    }
    qq.configureAuthSessionRepository(repository);
  } catch {
    memoryQqStore();
    // Rust must not treat this as "no saved login" and delete the Keychain item: the file may only
    // be temporarily unreadable. Keep NetEase and in-session QQ working, and report separately.
    console.log('SONG_DOWNLOAD_QQ_STORE_ERROR=1');
  }
}

function tokenFromCookie(cookie) {
  if (typeof cookie !== 'string') return null;
  for (const part of cookie.split(';')) {
    const trimmed = part.trim();
    const equals = trimmed.indexOf('=');
    if (equals > 0 && trimmed.slice(0, equals) === 'qqmusic_session') {
      return trimmed.slice(equals + 1);
    }
  }
  return null;
}

async function isQqSessionPersisted(cookie) {
  const token = tokenFromCookie(cookie);
  if (!token) return null;
  if (!qqFileStore) return false;
  if (!qqBackend) throw new Error('QQ persistence is unavailable');
  const repository = qqBackend.createConfiguredAuthSessionRepository({
    QQ_AUTH_SESSION_PATH: qqFileStore.path,
    QQ_SESSION_SECRET: qqFileStore.secret,
  });
  if (!repository) throw new Error('QQ persistence is unavailable');
  const sessions = repository.load();
  if (!Array.isArray(sessions)) throw new Error('QQ persistence state is invalid');
  return sessions.some((session) => session && session.token === token);
}

async function callNetease(name, params, cookie) {
  try {
    return (await netease[name]({ ...params, cookie })).body;
  } catch (error) {
    // Some Netease business errors, including pending QR codes, reject with a body.
    if (error && error.body && typeof error.body === 'object') return error.body;
    throw error;
  }
}

function json(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  res.end(JSON.stringify(body));
}

async function readBody(req) {
  let text = '';
  req.setEncoding('utf8');
  for await (const chunk of req) {
    text += chunk;
    if (text.length > 16384) throw new Error('Request too large');
  }
  return JSON.parse(text);
}

async function check() {
  if (qq.server || typeof qq.default.callback !== 'function' ||
      [...neteaseRoutes.values()].some((name) => typeof netease[name] !== 'function')) {
    throw new Error('Unsupported provider package exports');
  }
  const methods = qqRouteMethods();
  for (const route of qqRoutes) {
    // The pinned package exposes every allowlisted QQ route as GET. A POST here would need a
    // different forwarding path, so fail the standalone check instead of calling it as GET.
    if (methods.get(route) !== 'GET') throw new Error('Unsupported QQ route method');
  }
  loadQqBackend();
  // The Netease QR-create module is local: exercise a real adapter call without a listener or upstream access.
  const result = await callNetease('login_qr_create', { key: 'check', qrimg: 'true' }, '');
  if (!result?.data?.qrimg?.startsWith('data:image/png;base64,')) throw new Error('QR generation failed');
  console.log('Provider adapters ready');
}

function start() {
  const secret = Buffer.from(process.env.SONG_DOWNLOAD_BRIDGE_TOKEN || '');
  if (secret.length < 32 || qq.server) throw new Error('Invalid bridge configuration');
  delete process.env.SONG_DOWNLOAD_BRIDGE_TOKEN;
  const callQq = qq.default.callback();
  const server = createServer(async (req, res) => {
    const token = Buffer.from(req.headers.authorization || '');
    if (req.method !== 'POST' || token.length !== secret.length || !timingSafeEqual(token, secret)) {
      return json(res, 403, { code: 403 });
    }
    // Internal, authenticated liveness check for the encrypted QQ session file. A false result
    // means "configured but not persisted"; a 502 means the file could not be read right now.
    if (req.url === '/session/persisted') {
      try {
        const { cookie } = await readBody(req);
        const persisted = await isQqSessionPersisted(cookie);
        if (persisted === null) return json(res, 400, { code: 400 });
        return json(res, 200, { persisted });
      } catch {
        if (!res.headersSent) json(res, 502, { code: 502 });
        else res.end();
        return;
      }
    }
    const slash = req.url.indexOf('/', 1);
    const provider = req.url.slice(1, slash);
    const route = req.url.slice(slash);
    const name = provider === 'netease' && neteaseRoutes.get(route);
    if (!name && !(provider === 'qq' && qqRoutes.has(route))) {
      return json(res, 404, { code: 404 });
    }
    try {
      const { params, cookie } = await readBody(req);
      if (name) return json(res, 200, await callNetease(name, params, cookie));
      const query = new URLSearchParams(params);
      req.method = 'GET';
      req.url = `${route}?${query}`;
      delete req.headers['content-type'];
      delete req.headers['content-length'];
      delete req.headers.authorization;
      if (cookie) req.headers.cookie = cookie;
      return await callQq(req, res);
    } catch {
      if (!res.headersSent) json(res, 502, { code: 502 });
      else res.end();
    }
  });
  server.listen(0, '127.0.0.1', () => console.log(`SONG_DOWNLOAD_PORT=${server.address().port}`));
}

async function main() {
  if (process.argv[2] === '--check') {
    try {
      loadProviders();
      await check();
    } catch {
      console.error('Provider adapter check failed');
      process.exitCode = 1;
    }
    return;
  }

  // EOF always means the parent is gone, including during startup before a configuration line.
  process.stdin.on('end', () => process.exit(0));

  let config;
  try {
    config = await readStartupConfig();
  } catch {
    fail('invalid_startup_config');
  }

  try {
    if (config) {
      // The package configures itself from these documented variables at require time, so they
      // must exist just for that import and are removed in the finally block.
      process.env.QQ_AUTH_SESSION_PATH = config.path;
      process.env.QQ_SESSION_SECRET = config.secret;
    }
    try {
      loadProviders();
    } finally {
      delete process.env.QQ_AUTH_SESSION_PATH;
      delete process.env.QQ_SESSION_SECRET;
    }
    configureQqStore(config);
    start();
  } catch {
    fail('bridge_start_failed');
  }
}

main();
