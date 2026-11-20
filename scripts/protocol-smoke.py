"""Drive the real executable over JSONL using synthetic browser, database and host effects."""
import argparse
from contextlib import closing
import json
from pathlib import Path
import queue
import sqlite3
import subprocess
import sys
import tempfile
import threading
import uuid

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = json.loads((ROOT / 'extension.json').read_bytes())


class Host:
    def __init__(self, binary, root, platform, fixtures):
        self.process = subprocess.Popen([str(binary), *(['--fixtures', str(root)] if fixtures else [])],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.frames = queue.Queue()
        self.effects = []
        self.confirm = 1
        self.on_confirm = None
        self.tray = None
        self.reserved = set()
        def read():
            try:
                for line in self.process.stdout:
                    self.frames.put(json.loads(line))
            finally:
                self.frames.put(None)
        threading.Thread(target=read, daemon=True).start()
        hello = self.frames.get(timeout=15)
        assert {key:hello[key] for key in ('type','id','version','protocol')} == {'type':'hello','id':MANIFEST['id'],'version':MANIFEST['version'],'protocol':1}
        for name in ('package-data', 'package-temp'):
            (root / name).mkdir(exist_ok=True)
        self.rpc('initialize', dict(protocol=1, extensionId=MANIFEST['id'], platform=platform,
            packageDir=str(ROOT), dataDir=str(root / 'package-data'), tempDir=str(root / 'package-temp'),
            grantedCapabilities=MANIFEST['capabilities'], migrationPaths={}, reason='explicit synthetic test'))

    def send(self, frame):
        self.process.stdin.write((json.dumps(frame) + '\n').encode())
        self.process.stdin.flush()

    def effect(self, method, params):
        self.effects.append((method, params))
        if method == 'host.selection.read':
            return 'fixture selected words'
        if method == 'host.dialog.confirm':
            if self.on_confirm:
                self.on_confirm()
            return {'response':self.confirm}
        if method == 'host.apps.list':
            return [dict(name='Fixture Safari', bundleId='com.apple.Safari', path='/Synthetic/Safari.app'),
                    dict(name='Unrelated', bundleId='org.invalid.test', path='/Synthetic/Other.app')]
        if method == 'host.quicklinks.create':
            return {'status':'cancelled'}
        if method == 'host.tray.set':
            self.tray = params
        elif method == 'host.tray.remove':
            self.tray = None
        elif method == 'host.background.retain':
            assert params['durable'] is True
            self.reserved.add(params['key'])
        elif method == 'host.background.release':
            self.reserved.discard(params['key'])
        else:
            assert method in ['host.clipboard.write', 'host.window.visibility', 'host.shell.open', 'host.shell.reveal',
                              'host.shell.openWithApp', 'host.settings.open'], method
        return True

    def rpc(self, method, params, *, error=False):
        ident = f'h:{uuid.uuid4()}'
        self.send(dict(type='request',id=ident,method=method,params=params))
        while True:
            frame = self.frames.get(timeout=20)
            if frame is None:
                message = self.process.stderr.read().decode(errors='replace')
                raise AssertionError(f'Executable exited before its response: {message}')
            if frame['type'] == 'request':
                result = self.effect(frame['method'], frame['params'])
                self.send(dict(type='response',id=frame['id'],result=result))
            elif frame['type'] == 'response':
                assert frame['id'] == ident, frame
                assert ('error' in frame) == error, frame
                return frame['error']['message'] if error else frame['result']
            else:
                assert frame['type'] in ('event', 'cancel'), frame

    def open(self, command, sid=None, prefs=None, args=None, **extra):
        return self.rpc('session.open', dict(sessionId=sid or command, commandName=command,
            preferences=prefs or {}, argumentValues=args or {}, **extra))

    def call(self, sid, operation, error=False, **payload):
        return self.rpc('session.call', dict(sessionId=sid, operation=operation, payload=payload), error=error)

    def action(self, sid, action, error=False, **payload):
        return self.call(sid, 'extension:invoke-action', error=error, actionId=action, **payload)

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=20)
        assert self.process.returncode == 0, self.process.stderr.read().decode(errors='replace')


def setup(root):
    (root / '.tezbar-arc-fixture').write_bytes(b'tezbar-arc-fixture-v1\n')
    (root / 'suggestions.json').write_text(json.dumps(['text',['github.example','more fixture'],['Fixture navigation',''],[],
        {'google:suggesttype':['NAVIGATION','QUERY']}]), encoding='utf-8')
    target = root / 'fixture_file_100%.txt'
    target.write_bytes(b'explicitly disposable download')
    for profile, name, title, stamp in [
        ('Default','Primary',"Fixture 100% O'Reilly",11644473601000000), ('Profile 1','Work','Fixture Work',11644473602000000),
        ('Profile 2','__ARC_SYSTEM_PROFILE','Fixture internal',11644473603000000), ('Profile 4','After gap','Fixture skipped',11644473604000000),
    ]:
        directory = root / 'User Data' / profile
        directory.mkdir(parents=True)
        (directory / 'Preferences').write_text(json.dumps({'profile':{'name':name}}), encoding='utf-8')
        with closing(sqlite3.connect(directory / 'History')) as db:
            db.execute('CREATE TABLE urls (id INTEGER, title TEXT, url TEXT, last_visit_time INTEGER)')
            db.execute('INSERT INTO urls VALUES (1,?,?,?)', (title, f'https://fixture.invalid/{profile.split()[0]}_path', stamp))
            db.execute('CREATE TABLE downloads (id INTEGER, current_path TEXT, target_path TEXT, tab_url TEXT, end_time INTEGER)')
            db.execute('INSERT INTO downloads VALUES (1,?,?,?,?)', (str(target), str(target), 'https://fixture.invalid/source', stamp))
            db.commit()
    def tab(tab_id, title, location, spaces):
        return dict(id=tab_id, title=title, location=location, url='https://fixture.invalid/same-url', spaces=spaces)
    front = dict(key={'process':'123:123','window':'original-window'}, activeSpace='s1', incognito=False,
        spaces=[dict(id='s1',title='Work'),dict(id='s2',title='Personal')], tabs=[
            tab('favorite','Fixture Favorite','topApp',['s1','s2']), tab('pin','Fixture Pin','pinned',['s2']),
            tab('a','Fixture A','unpinned',['s1']), tab('b','Fixture B','unpinned',['s2']),
            tab('chinese','\u4e2d\u6587','unpinned',['s1'])])
    other = dict(key={'process':'123:123','window':'other-window'}, activeSpace='s3', incognito=False,
        spaces=[dict(id='s3',title='Other')], tabs=[tab('other','Wrong target','unpinned',['s3'])])
    return front, other


def save(root, windows):
    (root / 'browser.json').write_text(json.dumps(windows), encoding='utf-8')


def journal(root, kind=None):
    path = root / 'actions.jsonl'
    frames = [json.loads(line) for line in path.read_bytes().splitlines()] if path.exists() else []
    return [frame for frame in frames if kind is None or frame['kind'] == kind]


def nodes(root):
    yield root
    for child in root.get('children', ()):
        yield from nodes(child)


def rows(view):
    assert view['mode'] == 'view' and view['root']['type'] == 'List'
    return [node['props'] for node in nodes(view['root']) if node['type'] == 'List.Item' and not node['props']['id'].startswith('warning:')]


def titles(view):
    return [row['title'] for row in rows(view)]


def action(view, title, row=None):
    ids = next(x for x in rows(view) if x['title'] == row)['actionIds'] if row else None
    return next(a['id'] for a in view['actions'] if a['title'] == title and (ids is None or a['id'] in ids))


def live_fixture(binary, root, checks):
    front, other = setup(root)
    save(root, [front, other])
    originals = {path:path.read_bytes() for path in (root / 'User Data').rglob('*') if path.is_file()}
    host = Host(binary, root, 'arc-fixture', True)
    view = host.open('search-history')
    assert titles(view) == ['Fixture Work', "Fixture 100% O'Reilly"]
    copy_before = action(view, 'Copy URL', 'Fixture Work')
    view = host.call('search-history','extension:search-text-changed',searchText="100% O'Reilly Default_path")
    assert titles(view) == ["Fixture 100% O'Reilly"]
    assert 'Stale' in host.action('search-history', copy_before, error=True)
    host.action('search-history', action(view,'Copy URL'))
    assert host.effects[-1] == ('host.clipboard.write', {'text':'https://fixture.invalid/Default_path'})
    checks.append('Default and numbered history profiles, profile gap/internal exclusion, bound literal wildcard/quote search and clipboard RPC')

    tabs = host.open('search-tabs')
    assert titles(tabs) == ['Fixture A','Fixture B','\u4e2d\u6587']
    old = action(tabs,'Close Tab','Fixture B')
    space = host.open('search-space-tabs')
    assert titles(space) == ['Fixture A','\u4e2d\u6587']
    assert 'Stale' in host.action('search-space-tabs', old, error=True)
    pinyin = host.call('search-space-tabs','extension:search-text-changed',searchText='zhongwen')
    assert titles(pinyin) == ['\u4e2d\u6587']
    reversed_front = {**front,'tabs':front['tabs'][::-1]}
    save(root, [other, reversed_front])
    host.action('search-tabs', old)
    effect = journal(root,'tab')[-1]
    assert effect['action'] == 'close' and effect['tab'] == 'b' and effect['key']['window'] == 'original-window', effect
    checks.append('current-Space filter and pinyin, revision and session fencing, duplicate URLs survive tab/window reorder and navigation stales a destructive action')

    save(root, [front, other])
    unmodified = host.open('search-tabs','changed-tabs')
    stale = action(unmodified, 'Reload Tab', 'Fixture B')
    navigated = {**front, 'tabs':[{**t, 'url':'https://fixture.invalid/navigated'} if t['id']=='b' else t for t in front['tabs']]}
    save(root, [navigated, other])
    assert 'changed' in host.action('changed-tabs', stale, error=True)
    assert journal(root,'tab')[-1]['action'] == 'close'
    save(root, [front, other])
    fresh = host.open('search-tabs','fresh-tabs')
    dropdown = fresh['root']['props']['searchBarAccessory']['props']['actionId']
    all_tabs = host.action('fresh-tabs', dropdown, formValues={'value':'all'})
    assert titles(all_tabs) == ['Fixture Favorite','Fixture Pin','Fixture A','Fixture B','\u4e2d\u6587']
    assert titles(host.open('search-tabs','retained-tabs')) == titles(all_tabs)
    host.action('fresh-tabs', action(all_tabs,'Create Quick Link','Fixture Pin'))
    assert host.effects[-1] == ('host.quicklinks.create', {'link':'https://fixture.invalid/same-url','name':'Fixture Pin'})
    checks.append('Favorites and Pinned UI sections, stored dropdown filter, Quick Link cancellation through host')

    combined = host.open('search', prefs={'sorting':'tabsHistorySuggestions','showUnpinnedTabs':False}, searchText='fixture')
    assert [x['props']['title'] for x in combined['root']['children'] if x['type']=='List.Section'] == ['Favorites','Pinned Tabs','History','Suggestions']
    assert 'Fixture navigation' in titles(combined)
    pick = host.action('search', action(combined, 'Open in Space', 'Fixture Pin'))
    assert titles(pick) == ['Work','Personal']
    host.action('search', action(pick,'Open Here','Personal'))
    assert journal(root,'new-tabs')[-1]['space'] == 's2'
    combined = host.open('search','other-browser',prefs={'showHistory':False},searchText='fixture')
    pick = host.action('other-browser', action(combined,'Open in Other Browser','Fixture Favorite'))
    assert titles(pick) == ['Fixture Safari']
    host.action('other-browser', action(pick,'Open Here','Fixture Safari'))
    assert ('host.shell.openWithApp', {'path':'https://fixture.invalid/same-url','appPath':'/Synthetic/Safari.app'}) in host.effects
    checks.append('merged search order and toggles, remote suggestion payload and destination pickers')

    space = host.open('search-spaces')
    assert rows(space)[0]['accessories'] == [{'tag':'Active'}]
    host.action('search-spaces', action(space,'Open Space','Personal'))
    assert journal(root,'space')[-1]['space'] == 's2'
    download = host.open('search-downloads',searchText='file_100%')
    assert titles(download) == ['fixture_file_100%.txt']
    host.action('search-downloads', action(download,'Show in Finder'))
    assert ('host.shell.reveal', {'path':str(root / 'fixture_file_100%.txt')}) in host.effects
    checks.append('Space selection and Default-profile download reveal')

    host.open('new-window', args={'space':'2'})
    host.open('new-blank-window')
    host.open('new-little-arc', prefs={'engine':'duckduckgo'})
    host.open('new-incognito-window', args={'url':'fixture words'}, prefs={'engine':'kagi'})
    host.open('new-tab', args={'url':'example.org,localhost:8080/a','space':'Work'})
    host.open('release-notes')
    host.open('boost-gallery')
    host.open('new-easel')
    assert journal(root,'new-window')[-2]['space'] == 's2'
    assert journal(root,'new-window')[-1]['url'] == 'https://kagi.com/search?q=fixture%20words'
    assert journal(root,'little')[-1]['url'] == 'https://www.duckduckgo.com/?q=fixture%20selected%20words'
    assert [event['menu'] for event in journal(root,'menu')] == ['blank-window','easel']
    assert journal(root,'new-tabs')[-3]['urls'] == ['https://example.org/','https://localhost:8080/a']
    assert 'Release-Notes' in journal(root,'new-tabs')[-2]['urls'][0]
    assert journal(root,'new-tabs')[-1]['urls'] == ['https://arc.net/boosts']
    checks.append('eight Arc launch/menu commands and argument, preference, selection, engine and URL order')

    host.confirm = 0
    host.open('reset-space-tabs','cancel-reset')
    assert not journal(root,'reset')
    host.confirm = 1
    host.on_confirm = lambda: save(root, [{**front,'activeSpace':'s2'}, other])
    params = dict(sessionId='stale-reset',commandName='reset-space-tabs',preferences={},argumentValues={})
    assert 'changed' in host.rpc('session.open',params,error=True)
    assert not journal(root,'reset')
    host.on_confirm = None
    save(root, [front, other])
    host.open('reset-space-tabs','confirm-reset')
    assert journal(root,'reset')[-1]['space'] == 's1'
    checks.append('reset cancellation, confirmation and active Space revalidation')

    host.open('menu-bar')
    tray = host.tray
    assert any(item.get('commandName') == 'search-tabs' for item in tray['items'])
    assert any(item.get('commandName') == 'search-spaces' for item in tray['items'])
    favorite = next(item['id'] for item in tray['items'] if item['title']=='Fixture Favorite')
    host.rpc('background.event',dict(resourceId='arc-sidebar',actionId=favorite))
    assert journal(root,'tab')[-1]['tab'] == 'favorite'
    save(root, [])
    refresh = next(item['id'] for item in tray['items'] if item['title']=='Refresh Arc Menu')
    host.rpc('background.event',dict(resourceId='arc-sidebar',actionId=refresh))
    assert journal(root,'snapshot')[-1]['launch'] is False
    assert not any(item['title']=='Fixture Favorite' for item in host.tray['items'])
    assert 'Stale' in host.rpc('background.event',dict(resourceId='arc-sidebar',actionId=favorite),error=True)
    disable = next(item['id'] for item in host.tray['items'] if item['title']=='Hide Arc Menu')
    host.rpc('background.event',dict(resourceId='arc-sidebar',actionId=disable))
    assert not host.reserved and host.tray is None
    host.close()
    host = Host(binary, root, 'arc-fixture', True)
    host.rpc('background.activate',dict(key='arc-sidebar',data={}))
    assert host.tray is not None and host.reserved == {'arc-sidebar'}
    assert journal(root,'snapshot')[-1]['launch'] is False
    host.close()
    checks.append('tray ownership, Favorites, direct view links, stale clicks, passive refresh/restore and explicit removal')

    assert {path:path.read_bytes() for path in (root / 'User Data').rglob('*') if path.is_file()} == originals
    checks.append('fixture history, downloads and Preferences remain byte-identical')


def unsupported(binary, root, checks):
    for fixture in [False, True]:
        host = Host(binary, root, 'x86_64-pc-windows-msvc', fixture)
        for command in MANIFEST['commands']:
            params = dict(sessionId=command['name'], commandName=command['name'], preferences={}, argumentValues={})
            message = host.rpc('session.open', params, error=True)
            assert ('requires the arc-fixture platform' if fixture else 'supports macOS only') in message, message
        assert not host.effects
        host.close()
    checks.append('all 16 commands reject real Windows context with and without explicit fixture switch')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    parser.add_argument('--report', type=Path, default=ROOT / 'reports/protocol-smoke.json')
    args = parser.parse_args()
    checks = []
    with tempfile.TemporaryDirectory(prefix='tezbar-arc-fixture-') as temp:
        root = Path(temp).resolve()
        live_fixture(args.binary.resolve(), root, checks)
        unsupported(args.binary.resolve(), root, checks)
    report = {'binary':args.binary.name,'checks':checks,'passed':len(checks),'os':sys.platform,
              'limitation':'JSONL, Arc data and every desktop/browser/HTTP effect are synthetic.'}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(report, indent=2) + '\n'
    args.report.write_bytes(body.encode())
    print(body, end='')


if __name__ == '__main__':
    main()
