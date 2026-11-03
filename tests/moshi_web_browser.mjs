// Browser-side smoke test; Node 22+ and a Chromium-based browser are required.
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, appendFile, writeFile, rename } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'));
const chrome = process.env.CHROME_BIN;
if (!chrome || typeof WebSocket !== 'function') throw new Error('CHROME_BIN and Node 22+ are required');
const profile = await mkdtemp(path.join(os.tmpdir(), 'neordr-chrome-'));
const browser = spawn(chrome, [
  '--headless=new', '--no-first-run', '--no-default-browser-check', '--disable-gpu',
  '--disable-background-networking', '--disable-extensions',
  '--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1, EXCLUDE localhost',
  '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank',
], { stdio: 'ignore' });
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function eventually(label, fn, timeout = 20000) {
  const until = Date.now() + timeout;
  let last;
  while (Date.now() < until) {
    try { if (await fn()) return; } catch (error) { last = error; }
    await delay(150);
  }
  throw new Error(`${label} timed out${last ? `: ${last.message}` : ''}`);
}
let ws;
try {
  let port;
  await eventually('Chrome startup', async () => {
    if (browser.exitCode !== null) throw new Error(`Chrome exited ${browser.exitCode}`);
    try { port = (await readFile(path.join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; }
    catch { return false; }
    return !!port;
  });
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let sequence = 0;
  const pending = new Map();
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) reject(new Error(message.error.message)); else resolve(message.result);
    } else if (message.method === 'Fetch.requestPaused') {
      const request = message.params;
      const allowed = request.request.url.startsWith(fixture.url + '/');
      command(allowed ? 'Fetch.continueRequest' : 'Fetch.failRequest',
        allowed ? { requestId: request.requestId } : { requestId: request.requestId, errorReason: 'BlockedByClient' }).catch(() => {});
    }
  };
  function command(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++sequence;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async function evaluate(expression) {
    const result = await command('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
    return result.result.value;
  }
  async function waitFor(label, expression) {
    await eventually(label, () => evaluate(`Boolean(${expression})`));
  }
  async function click(expression) {
    const clicked = await evaluate(`(() => { const e = ${expression}; if (!e) return false; e.click(); return true; })()`);
    if (!clicked) throw new Error(`Missing control: ${expression}`);
  }
  await command('Page.enable');
  await command('Runtime.enable');
  // Block every page request outside the test proxy, including external fonts.
  await command('Fetch.enable', { patterns: [{ urlPattern: '*' }] });
  await command('Page.navigate', { url: fixture.url + '/' });
  await waitFor('local fixture gateway', `[...document.querySelectorAll('button')].some(e => e.textContent.includes('(local)'))`);
  await click(`[...document.querySelectorAll('button')].find(e => e.textContent.includes('(local)'))`);
  for (const agent of ['codex', 'claude']) {
    const marker = fixture.sessions[agent].marker;
    await waitFor(`${agent} sidebar`, `[...document.querySelectorAll('a')].some(e => e.textContent.includes(${JSON.stringify(marker)}))`);
  }
  await waitFor('isolated host discovery', `document.body.innerText.includes('no simulators streaming') && document.body.innerText.includes('no dev servers detected')`);
  await click(`[...document.querySelectorAll('a')].find(e => e.textContent.includes(${JSON.stringify(fixture.sessions.codex.marker)}))`);
  await waitFor('Codex composer', `document.querySelector('[data-composer-input="true"]')`);
  await waitFor('Codex chat selection', `new URL(location.href).searchParams.get('session') === ${JSON.stringify(fixture.sessions.codex.session)}`);
  console.log('Moshi web: synthetic agents in sidebar and Codex chat selection: PASS');

  const codexInput = path.join(fixture.root, 'codex.input');
  const claudeInput = path.join(fixture.root, 'claude.input');
  const beforeCodex = await readFile(codexInput);
  const beforeClaude = await readFile(claudeInput);
  const message = `NEORDR_BROWSER_${crypto.randomBytes(6).toString('hex')}`;
  await evaluate(`document.querySelector('[data-composer-input="true"]').focus()`);
  await command('Input.insertText', { text: message });
  await waitFor('enabled send button', `(() => { const b = document.querySelector('button[aria-label="Send"]'); return b && !b.disabled; })()`);
  await click(`document.querySelector('button[aria-label="Send"]')`);
  await eventually('Codex prompt delivery', async () => {
    const input = await readFile(codexInput);
    return input.length > beforeCodex.length && input.subarray(beforeCodex.length).includes(Buffer.from(message));
  });
  if (!(await readFile(claudeInput)).equals(beforeClaude)) throw new Error('Codex prompt reached the Claude pane');
  console.log('Moshi web: composer sends to the selected Codex pane only: PASS');

  const streamMarker = `NEORDR_WEB_STREAM_${crypto.randomBytes(6).toString('hex')}`;
  await appendFile(fixture.sessions.codex.transcript, JSON.stringify({
    type: 'response_item', timestamp: new Date().toISOString(), payload: {
      type: 'message', role: 'user', content: [{ type: 'input_text', text: streamMarker }],
    },
  }) + '\n');
  await waitFor('streamed Codex transcript', `document.body.innerText.includes(${JSON.stringify(streamMarker)})`);
  await command('Page.reload');
  await waitFor('transcript after reload', `document.body.innerText.includes(${JSON.stringify(streamMarker)})`);
  console.log('Moshi web: transcript append and reload: PASS');

  await waitFor('Claude sidebar after reload', `[...document.querySelectorAll('a')].some(e => e.textContent.includes(${JSON.stringify(fixture.sessions.claude.marker)}))`);
  await click(`[...document.querySelectorAll('a')].find(e => e.textContent.includes(${JSON.stringify(fixture.sessions.claude.marker)}))`);
  await waitFor('Claude chat selection', `new URL(location.href).searchParams.get('session') === ${JSON.stringify(fixture.sessions.claude.session)}`);
  await waitFor('Claude composer', `document.querySelector('[data-composer-input="true"]')`);
  const beforeClaudePrompt = await readFile(claudeInput);
  const beforeCodexPrompt = await readFile(codexInput);
  const claudeMessage = `NEORDR_BROWSER_CLAUDE_${crypto.randomBytes(6).toString('hex')}`;
  await evaluate(`document.querySelector('[data-composer-input="true"]').focus()`);
  await command('Input.insertText', { text: claudeMessage });
  await waitFor('enabled Claude send button', `(() => { const b = document.querySelector('button[aria-label="Send"]'); return b && !b.disabled; })()`);
  await click(`document.querySelector('button[aria-label="Send"]')`);
  await eventually('Claude prompt delivery', async () => {
    const input = await readFile(claudeInput);
    return input.length > beforeClaudePrompt.length && input.subarray(beforeClaudePrompt.length).includes(Buffer.from(claudeMessage));
  });
  if (!(await readFile(codexInput)).equals(beforeCodexPrompt)) throw new Error('Claude prompt reached the Codex pane');
  console.log('Moshi web: switching agents routes Claude input to its own pane: PASS');

  await click(`[...document.querySelectorAll('button')].find(e => e.textContent.trim() === 'Term')`);
  await waitFor('terminal input', `document.querySelector('textarea[aria-label="Terminal input"]')`);
  const beforeTerminal = await readFile(claudeInput);
  const terminalMessage = `NEORDR_WEB_TERMINAL_${crypto.randomBytes(6).toString('hex')}`;
  await evaluate(`document.querySelector('textarea[aria-label="Terminal input"]').focus()`);
  await command('Input.insertText', { text: terminalMessage });
  await eventually('terminal input delivery', async () => {
    const input = await readFile(claudeInput);
    return input.length > beforeTerminal.length && input.subarray(beforeTerminal.length).includes(Buffer.from(terminalMessage));
  });
  console.log('Moshi web: terminal view sends input to the selected pane: PASS');

  await waitFor('Codex sidebar after terminal input', `[...document.querySelectorAll('a')].some(e => e.textContent.includes(${JSON.stringify(fixture.sessions.codex.marker)}))`);
  await click(`[...document.querySelectorAll('a')].find(e => e.textContent.includes(${JSON.stringify(fixture.sessions.codex.marker)}))`);
  await waitFor('return to Codex chat', `new URL(location.href).searchParams.get('session') === ${JSON.stringify(fixture.sessions.codex.session)}`);
  const action = `approval-${crypto.randomBytes(6).toString('hex')}`;
  const commandText = 'printf neordr-web-approval';
  const event = {
    hook_event_name: 'PermissionRequest', tool_name: 'exec_command',
    tool_use_id: action, action_id: action,
    tool_input: { command: commandText, cmd: commandText, description: commandText },
    session_id: fixture.sessions.codex.session, cwd: fixture.root,
    transcript_path: fixture.sessions.codex.transcript,
  };
  const control = {
    id: action, title: 'Action Required', event,
    screen: `Would you like to run the following command?\n$ ${commandText}\n1. Yes, proceed (y)\n2. No, and tell Codex what to do differently (esc)`,
  };
  const controlPath = path.join(fixture.root, 'codex.control');
  await writeFile(controlPath + '.tmp', JSON.stringify(control));
  await rename(controlPath + '.tmp', controlPath);
  await eventually('synthetic permission screen', async () => {
    try { return (await readFile(path.join(fixture.root, 'codex.ack'), 'utf8')) === action; }
    catch { return false; }
  });
  await waitFor('approval card', `document.body.innerText.includes(${JSON.stringify(commandText)}) && [...document.querySelectorAll('button')].some(e => e.textContent.trim() === 'Approve')`);
  const beforeApproval = await readFile(codexInput);
  const beforeOtherApproval = await readFile(claudeInput);
  await click(`[...document.querySelectorAll('button')].find(e => e.textContent.trim() === 'Approve')`);
  await eventually('approval delivered once', async () => (await readFile(codexInput)).equals(Buffer.concat([beforeApproval, Buffer.from('y')])));
  await delay(300);
  if (!(await readFile(codexInput)).equals(Buffer.concat([beforeApproval, Buffer.from('y')])) ||
      !(await readFile(claudeInput)).equals(beforeOtherApproval)) throw new Error('approval was duplicated or reached the wrong pane');
  const completed = {
    id: action + '-completed', title: '⠋ Working',
    event: { ...event, hook_event_name: 'PostToolUse' },
    screen: '• Working (3s • esc to interrupt)\n› ',
  };
  await writeFile(controlPath + '.tmp', JSON.stringify(completed));
  await rename(controlPath + '.tmp', controlPath);
  await eventually('synthetic approval completion', async () => (await readFile(path.join(fixture.root, 'codex.ack'), 'utf8')) === completed.id);
  await waitFor('approval card cleared', `![...document.querySelectorAll('button')].some(e => e.textContent.trim() === 'Approve') && !document.body.innerText.includes('Submitting')`);
  await command('Page.reload');
  await waitFor('chat after approval reconnect', `document.querySelector('[data-composer-input="true"]') && document.body.innerText.includes(${JSON.stringify(streamMarker)})`);
  await delay(1000); // Allow the reconnected agent-status watch to deliver a stale card.
  if (await evaluate(`[...document.querySelectorAll('button')].some(e => e.textContent.trim() === 'Approve') || document.body.innerText.includes('Submitting')`)) throw new Error('approval card returned after reload');
  console.log('Moshi web: approval delivered once, clears, and stays cleared after reload: PASS');
} finally {
  if (ws) ws.close();
  browser.kill('SIGTERM');
  await Promise.race([new Promise(resolve => browser.once('exit', resolve)), delay(3000)]);
  if (browser.exitCode === null) browser.kill('SIGKILL');
  await rm(profile, { recursive: true, force: true });
}
