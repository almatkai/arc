// Arc's Apple Events bridge. Input is JSON over stdin; there is no eval or shell.
// The indexed front window is read only to capture an identity, never to target a mutation.
const bundle = 'company.thebrowser.Browser';

function bridge(input, app, processToken, pause, clickMenu) {
  const stale = () => { throw Error('ARC_STALE'); };
  const idOK = value => Number.isSafeInteger(value) ||
    (typeof value === 'string' && value.length > 0 && value.length <= 256);
  const title = value => Array.from(String(value || '')).slice(0, 750).join('');
  const ids = (collection, max) => {
    const result = collection();
    if (result.length > max) throw Error('ARC_SIZE');
    return result.map(item => item.id()).filter(idOK);
  };
  const locate = (collection, native, max) => {
    if (!idOK(native) || ids(collection, max).filter(id => id === native).length !== 1) stale();
    const object = collection.byId(native);
    if (object.id() !== native) stale();
    return object;
  };
  const windowAt = key => {
    if (!key || processToken() !== key.process) stale();
    return locate(app.windows, key.window, 64);
  };
  const spaceAt = (win, id) => locate(win.spaces, id, 128);
  const checkProcess = key => { if (processToken() !== key.process) stale(); };
  const first = () => {
    const windows = app.windows();
    return windows.length ? windows[0].id() : null;
  };
  const ready = () => {
    if (!processToken()) {
      app.activate();
      for (let n = 0; n < 25 && !processToken(); n++) pause(0.1);
      if (!processToken()) throw Error('ARC_UNAVAILABLE');
      for (let n = 0; n < 8 && first() === null; n++) pause(0.1);
    } else if (first() === null) {
      app.Window().make();
    }
  };
  const activeSpace = win => {
    try { return win.activeSpace.id(); } catch (_) { return null; }
  };
  const done = {done: true};
  if (input.kind === 'snapshot') {
    if (input.launch) ready();
    const process = processToken();
    const window = process ? first() : null;
    if (window === null) return {key: null, activeSpace: null, incognito: null, spaces: [], tabs: []};
    const key = {process, window};
    const win = windowAt(key);
    const active = activeSpace(win);
    const spaceIds = ids(win.spaces, 128);
    const membership = new Map();
    const spaces = spaceIds.map(id => {
      const space = win.spaces.byId(id);
      if (space.id() !== id) stale();
      if (input.tabs) {
        for (const tab of ids(space.tabs, 4096)) {
          const members = membership.get(tab) || [];
          members.push(id);
          membership.set(tab, members);
        }
      }
      return {id, title: title(space.title())};
    });
    if (new Set(spaceIds).size !== spaceIds.length) stale();
    const tabIds = input.tabs ? ids(win.tabs, 4096) : [];
    if (new Set(tabIds).size !== tabIds.length) stale();
    const tabs = tabIds.map(id => {
      const tab = win.tabs.byId(id);
      if (tab.id() !== id) stale();
      const url = String(tab.url() || '');
      return {id, url: url.length <= 8192 ? url : '', title: title(tab.title()),
        location: String(tab.location()), spaces: membership.get(id) || []};
    });
    let incognito = null;
    try { const mode = win.incognito(); if (typeof mode === 'boolean') incognito = mode; } catch (_) { /* older Arc dictionary */ }
    checkProcess(key);
    return {key, activeSpace: active, incognito, spaces, tabs};
  }
  if (input.kind === 'tab') {
    const tab = locate(windowAt(input.key).tabs, input.tab, 4096);
    if (input.expected_url !== null && String(tab.url() || '') !== input.expected_url) stale();
    checkProcess(input.key);
    if (input.action === 'close') tab.close();
    else if (input.action === 'reload') tab.reload();
    else if (input.action === 'select') { tab.select(); app.activate(); }
    else throw Error('ARC_UNSUPPORTED');
    return done;
  }
  if (input.kind === 'space') {
    const space = spaceAt(windowAt(input.key), input.space);
    checkProcess(input.key);
    space.focus();
    app.activate();
    return done;
  }
  if (input.kind === 'new-tabs') {
    const win = windowAt(input.key);
    const target = input.space === null ? win : spaceAt(win, input.space);
    for (const url of input.urls) {
      checkProcess(input.key);
      target.tabs.push(app.Tab({url}));
    }
    app.activate();
    return done;
  }
  if (input.kind === 'new-window') {
    let name = null;
    if (input.space !== null) name = String(spaceAt(windowAt(input.scope), input.space).title() || '');
    const win = app.Window({incognito: input.incognito}).make();
    if (input.space !== null) {
      const available = ids(win.spaces, 128);
      if (available.includes(input.space)) {
        spaceAt(win, input.space).focus();
      } else {
        const candidates = name ? available.filter(id => String(win.spaces.byId(id).title()) === name) : [];
        if (candidates.length !== 1) stale();
        spaceAt(win, candidates[0]).focus();
      }
    }
    if (input.url !== null) win.tabs.push(app.Tab({url: input.url}));
    app.activate();
    return done;
  }
  if (input.kind === 'little') {
    app.Tab({url: input.url}).make();
    app.activate();
    return done;
  }
  if (input.kind === 'menu') {
    if (input.menu === 'easel') ready();
    app.activate();
    pause(0.5);
    if (input.menu === 'blank-window') clickMenu('File', 'Blank window', () => {});
    else if (input.menu === 'easel') clickMenu('File', 'New Easel', () => {});
    else throw Error('ARC_UNSUPPORTED');
    return done;
  }
  if (input.kind === 'reset') {
    const selected = () => {
      if (processToken() !== input.key.process || first() !== input.key.window ||
          activeSpace(windowAt(input.key)) !== input.space) stale();
    };
    selected();
    app.activate();
    clickMenu('Tabs', 'Reset all tabs in this Space', selected);
    return done;
  }
  throw Error('ARC_UNSUPPORTED');
}

function run() {
  ObjC.import('AppKit');
  const stdin = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  const text = ObjC.unwrap($.NSString.alloc.initWithDataEncoding(stdin, $.NSUTF8StringEncoding));
  const input = JSON.parse(text);
  if (input.kind === 'version') {
    const application = $.NSWorkspace.sharedWorkspace.URLForApplicationWithBundleIdentifier(bundle);
    if (application.isNil()) return 'null';
    const version = $.NSBundle.bundleWithURL(application).objectForInfoDictionaryKey('CFBundleShortVersionString');
    return JSON.stringify(ObjC.unwrap(version) || null);
  }
  const processToken = () => {
    const matches = $.NSRunningApplication.runningApplicationsWithBundleIdentifier(bundle);
    if (Number(matches.count) !== 1) return null;
    const app = matches.objectAtIndex(0);
    const pid = Number(app.processIdentifier);
    const birth = Number(app.launchDate.timeIntervalSince1970);
    return pid > 0 && birth > 0 ? `${pid}:${birth}` : null;
  };
  const clickMenu = (menu, name, recheck) => {
    const system = Application('System Events');
    const process = system.applicationProcesses.whose({bundleIdentifier: bundle})();
    if (process.length !== 1 || !process[0].frontmost()) throw Error('ARC_MISSING_MENU');
    const item = process[0].menuBars[0].menuBarItems.byName(menu).menus[0].menuItems.byName(name);
    if (!item.exists() || !item.enabled()) throw Error('ARC_MISSING_MENU');
    recheck();
    item.click();
  };
  return JSON.stringify(bridge(input, Application(bundle), processToken, delay, clickMenu));
}
