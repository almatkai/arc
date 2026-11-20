const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const script = fs.readFileSync(path.join(__dirname, '../src/arc.js'), 'utf8');
const bridge = vm.runInNewContext(`${script}\nbridge`);

let processToken = '10:1';
const effects = [];
const keyed = (rows, factory, onLookup = () => {}) => {
  const collection = () => rows.map(item => factory(item.id));
  collection.byId = id => {
    onLookup();
    return factory(id);
  };
  return collection;
};
const member = (rows, id) => rows.find(item => item.id === id) || {id: null};
const makeTab = (rows, id) => ({
  id: () => member(rows, id).id,
  title: () => id,
  url: () => 'https://example.invalid/duplicate',
  location: () => 'unpinned',
  close: () => effects.push(['close', member(rows, id).id]),
  reload: () => effects.push(['reload', member(rows, id).id]),
  select: () => effects.push(['select', member(rows, id).id]),
});
const sample = [
  {id: 'W1', tabs: [{id: 'A'}, {id: 'B'}], spaces: [{id: 's1'}, {id: 's2'}], active: 's1'},
  {id: 'W2', tabs: [{id: 'C'}], spaces: [{id: 'other'}], active: 'other'},
];
const targetWindow = id => {
  const item = member(sample, id);
  const tabs = item.tabs || [];
  const spaces = item.spaces || [];
  return {
    id: () => item.id,
    incognito: () => false,
    activeSpace: {id: () => item.active},
    spaces: keyed(spaces, id => ({id: () => member(spaces, id).id,
      title: () => id, focus: () => effects.push(['focus', id]),
      tabs: keyed(tabs, id => makeTab(tabs, id))})),
    tabs: keyed(tabs, id => makeTab(tabs, id), () => tabs.reverse()),
  };
};
const app = {
  windows: keyed(sample, targetWindow),
  activate: () => effects.push(['activate']),
};
let click = (menu, label, recheck) => { recheck(); effects.push([menu, label]); };
const run = input => bridge(input, app, () => processToken, () => {}, click);
const key = {process: '10:1', window: 'W1'};
const target = {kind: 'tab', key, tab: 'B', action: 'close', expected_url: 'https://example.invalid/duplicate'};

const before = run({kind: 'snapshot', tabs: true, launch: false});
assert.equal(before.key.window, 'W1');
assert.equal(before.tabs[0].url, before.tabs[1].url);
assert.equal(before.tabs[1].spaces[0], 's1');
sample.reverse();
run(target);
assert.deepEqual(effects.splice(0), [['close', 'B']]);
assert.throws(() => run({...target, expected_url: 'https://stale.example/'}), /ARC_STALE/);
sample[1].tabs.push({id: 'B'});
assert.throws(() => run(target), /ARC_STALE/);
sample[1].tabs = [{id: 'A'}];
assert.throws(() => run(target), /ARC_STALE/);
processToken = '10:2';
assert.throws(() => run(target), /ARC_STALE/);
processToken = '10:1';
assert.deepEqual(effects, []);

sample.reverse();
run({kind: 'space', key, space: 's2'});
assert.deepEqual(effects.splice(0), [['focus', 's2'], ['activate']]);
click = (menu, label, recheck) => {
  sample[0].active = 's2';
  recheck();
  effects.push([menu, label]);
};
assert.throws(() => run({kind: 'reset', key, space: 's1'}), /ARC_STALE/);
assert.deepEqual(effects.splice(0), [['activate']]);
processToken = null;
const closed = run({kind: 'snapshot', tabs: true, launch: false});
assert.equal(closed.key, null);
assert.deepEqual(effects, []);
console.log('Arc scripted bridge mock passed: duplicate URLs, reorder, ambiguous/missing IDs, restart, reset fence, passive read.');
