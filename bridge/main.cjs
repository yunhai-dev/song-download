const { createServer } = require('node:http');
const { timingSafeEqual } = require('node:crypto');

// Embed QQ's Koa callback without letting the package open its own listener.
process.env.NODE_ENV = 'test';
process.env.LOG_LEVEL = 'error';
delete process.env.QQ_AUTH_SESSION_PATH;
delete process.env.QQ_SESSION_SECRET;
const qq = require('@yakult-green-tea/qq-music-api');
const netease = require('@neteasecloudmusicapienhanced/api');

const qqRoutes = new Set([
  '/login/qr/key', '/login/qr/create', '/login/qr/check', '/login/status',
  '/user/playlist', '/user/playlist-detail', '/getSongListDetail', '/getMusicPlay',
  '/user/liked-songs', '/getLyric',
]);
const neteaseRoutes = new Map([
  ['/login/qr/key', 'login_qr_key'],
  ['/login/qr/create', 'login_qr_create'],
  ['/login/qr/check', 'login_qr_check'],
  ['/login/status', 'login_status'],
  ['/user/account', 'user_account'],
  ['/user/playlist', 'user_playlist'],
  ['/playlist/track/all', 'playlist_track_all'],
  ['/song/url/v1', 'song_url_v1'],
  ['/lyric/new', 'lyric_new'],
]);

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
  // The Netease QR-create module is local: exercise a real adapter call without a listener or upstream access.
  const result = await callNetease('login_qr_create', { key: 'check', qrimg: 'true' }, '');
  if (!result?.data?.qrimg?.startsWith('data:image/png;base64,')) throw new Error('QR generation failed');
  console.log('Provider adapters ready');
}

function start() {
  const secret = Buffer.from(process.env.SONG_DOWNLOAD_BRIDGE_TOKEN || '');
  if (secret.length < 32 || qq.server) throw new Error('Invalid bridge configuration');
  delete process.env.SONG_DOWNLOAD_BRIDGE_TOKEN;
  qq.configureAuthSessionRepository({ kind: 'memory', load: () => [], save: () => {} });
  const callQq = qq.default.callback();
  const server = createServer(async (req, res) => {
    const token = Buffer.from(req.headers.authorization || '');
    if (req.method !== 'POST' || token.length !== secret.length || !timingSafeEqual(token, secret)) {
      return json(res, 403, { code: 403 });
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
  // EOF also handles parent process termination without relying on Rust destructors.
  process.stdin.resume();
  process.stdin.on('end', () => process.exit(0));
}

if (process.argv[2] === '--check') {
  check().catch(() => { console.error('Provider adapter check failed'); process.exitCode = 1; });
} else {
  start();
}
