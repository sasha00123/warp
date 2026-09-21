import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name('sync-upstream.sh').read_text()

class SyncPolicy(unittest.TestCase):
    def git(self, directory, *args):
        return subprocess.check_output(['git','-C',str(directory),*args],stderr=subprocess.STDOUT,text=True).strip()
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory();self.addCleanup(self.temporary.cleanup)
        self.root=Path(self.temporary.name)
        self.upstream=self.root/'upstream';self.origin=self.root/'origin';self.checkout=self.root/'checkout'
        self.upstream.mkdir();self.origin.mkdir();self.checkout.mkdir()
        self.git(self.upstream,'init','-b','main');self.git(self.origin,'init','--bare')
        self.git(self.upstream,'config','user.name','Test');self.git(self.upstream,'config','user.email','test@example.invalid')
        (self.upstream/'source').write_text('base')
        self.git(self.upstream,'add','source');self.git(self.upstream,'commit','-m','base')
        self.base=self.git(self.upstream,'rev-parse','HEAD')
        self.git(self.upstream,'push',str(self.origin),'main','main:personal/main')
        self.git(self.checkout,'init','-b','main');self.git(self.checkout,'remote','add','origin',str(self.origin))
        (self.checkout/'distribution').mkdir()
        (self.checkout/'distribution/sync-upstream.sh').write_text(SCRIPT)
        (self.checkout/'distribution/config.json').write_text(json.dumps({'upstream':'zed-industries/zed','upstream_branch':'main'}))
        self.bin=self.root/'bin';self.bin.mkdir()
        (self.bin/'gh').write_text('#!/bin/sh\necho 1\n');(self.bin/'gh').chmod(0o755)
        self.env={**os.environ,'GH_REPO':'sasha00123/zed','PATH':str(self.bin)+os.pathsep+os.environ['PATH'],
          'GIT_CONFIG_COUNT':'1','GIT_CONFIG_KEY_0':f'url.{self.upstream}.insteadOf',
          'GIT_CONFIG_VALUE_0':'https://github.com/zed-industries/zed.git'}
    def sync(self):
        return subprocess.run(['bash','distribution/sync-upstream.sh'],cwd=self.checkout,env=self.env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
    def test_fast_forward_preserves_personal_branch(self):
        (self.upstream/'source').write_text('new upstream')
        self.git(self.upstream,'commit','-am','upstream update')
        result=self.sync();self.assertEqual(result.returncode,0,result.stdout)
        self.assertEqual(self.git(self.origin,'rev-parse','main'),self.git(self.upstream,'rev-parse','HEAD'))
        self.assertEqual(self.git(self.origin,'rev-parse','personal/main'),self.base)
    def test_divergence_never_overwrites_main(self):
        (self.upstream/'source').write_text('fork change');self.git(self.upstream,'commit','-am','fork')
        self.git(self.upstream,'push',str(self.origin),'main')
        fork=self.git(self.origin,'rev-parse','main')
        self.git(self.upstream,'reset','--hard',self.base)
        (self.upstream/'source').write_text('different upstream');self.git(self.upstream,'commit','-am','upstream')
        self.assertNotEqual(self.sync().returncode,0)
        self.assertEqual(self.git(self.origin,'rev-parse','main'),fork)
    def test_wrong_repository_is_rejected(self):
        self.env['GH_REPO']='someone/zed'
        self.assertNotEqual(self.sync().returncode,0)
        self.assertEqual(self.git(self.origin,'rev-parse','main'),self.base)
