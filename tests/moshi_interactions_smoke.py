#!/usr/bin/env python3
"""Moshi 0.4.9 composer and async-question input contracts, with synthetic agents.

Tests local terminal delivery, not model execution, upload storage, cloud approval
push or native-client UI. The approval_smoke suite covers plan/approval lifecycles.
"""
import tempfile
import re
import subprocess
from pathlib import Path
from approval_smoke import Rig
from moshi_smoke import HOOK


def composer(rig, source):
    rig.show(source, 'READY', {'hook_event_name':'UserPromptSubmit','prompt':'Composer fixture'})
    text='Unicode λ\n'+'\n'.join(f'line {i}: '+('x'*100) for i in range(20))
    rig.answer(source,'/v1/prompt',{'text':text},b'\x1b[200~'+text.encode()+b'\x1b[201~\r')
    # Moshi submits long pasted blocks separately from typed composer text.
    blocks=['First block\nλ', 'Second block\n'+'z'*1500]
    version = re.search(r'\b(\d+)\.(\d+)\.(\d+)\b', subprocess.check_output([HOOK, 'version'], text=True))
    assert version, 'Cannot determine the Moshi paste contract version'
    current_paste = tuple(map(int, version.groups())) >= (0, 4, 30)
    if current_paste:
        # The observed 0.4.30 contract separates later blocks and trailing text,
        # keeping those newlines inside bracketed paste instead of submitting them.
        parts = [blocks[0], '\n\n' + blocks[1], '\n\nExplain these blocks']
        expected = b''.join(b'\x1b[200~' + part.encode() + b'\x1b[201~' for part in parts) + b'\r'
    else:
        expected = b''.join(b'\x1b[200~'+b.encode()+b'\x1b[201~' for b in blocks)+b'Explain these blocks\r'
    rig.answer(source,'/v1/prompt',{'text':'Explain these blocks','pastedText':blocks},
               expected)
    # Uploaded files are represented by paths in composer text. Neordr must
    # deliver this literally, without expanding shell syntax or splitting spaces.
    attachment=rig.root/'attachment with spaces λ.txt';attachment.write_text('synthetic attachment')
    prompt=f'Inspect "{attachment}" and explain $(not-a-shell-command).'
    expected = prompt.encode()
    if source == 'codex' and current_paste:
        expected = b'\x1b[200~' + expected + b'\x1b[201~'
    rig.answer(source,'/v1/prompt',{'text':prompt},expected+b'\r')
    rig.answer(source,'/v1/keys',{'keys':['BTab','PageDn']},b'\x1b[Z\x1b[6~')
    rig.answer(source,'/v1/keys',{'keys':['Escape']},b'\x1b')
    rig.answer(source,'/v1/keys',{'keys':['not-a-real-key']},b'',status=400)
    print(f'{source}: long/Unicode prompt, multipart paste, literal attachment path, navigation and interrupt keys: PASS',flush=True)


def asynchronous_questions(rig):
    source='codex'
    questions=[{'id':'color','header':'Color','question':'Choose color','multiSelect':False,'options':['Blue','Green']},
               {'id':'size','header':'Size','question':'Choose size','multiSelect':False,'options':['Small','Large']}]
    tool='async-question-fixture'
    rig.show(source,'Question 1/2\nChoose color\n1. Blue\n2. Green\nEnter to submit',
             {'hook_event_name':'PreToolUse','tool_name':'request_user_input_async','tool_use_id':tool,
              'tool_input':{'questions':[{**q,'options':[{'label':o,'description':o} for o in q['options']]} for q in questions]}})
    body={'toolUseId':tool,'async':True,'questions':questions,
          'answers':[{'questionId':'color','optionIndexes':[1]}]}
    # A malformed partially answered payload must not inject even the first key.
    rig.answer(source,'/v1/questions/answer',body,b'',status=400)
    body['questions']=questions[:1]
    wrong={**body,'answers':[{'questionId':'other','optionIndexes':[1]}]}
    rig.answer(source,'/v1/questions/answer',wrong,b'',status=400)
    rig.answer(source,'/v1/questions/answer',body,b'2')
    # A later request contains only the still-unanswered visible question.
    # It must address that question, not replay the earlier answer.
    rig.show(source,'Question 2/2\nChoose size\n1. Small\n2. Large\nEnter to submit')
    rig.answer(source,'/v1/questions/answer',{'toolUseId':tool,'async':True,
               'questions':questions[1:],'answers':[{'questionId':'size','optionIndexes':[0]}]},b'1')
    print('Codex async questions: reject incomplete/mismatched answers, deliver remaining visible question without replay: PASS',flush=True)


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-interactions-',dir='/tmp') as directory:
        rig=Rig(Path(directory).resolve())
        try:
            rig.setup()
            for source in ['claude','codex']:rig.agent(source)
            for source in rig.agents:
                rig.resize(source,120)
                composer(rig,source)
            asynchronous_questions(rig)
        finally:rig.close()


if __name__=='__main__':main()
