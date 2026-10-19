const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ClaudeCodeResponseStream } = require('../gearbox-studio/lib/node/claude-code-response-stream');

const start = id => ({tool_calls:[{id,finished:false,function:{name:'gearbox_list_gears',arguments:'{"filter":"labs"}'}}]});

test('tool handler waits for its start frame to reach the consumer', async () => {
  const stream = new ClaudeCodeResponseStream();
  const seen = new Map();
  let calls = 0;
  const invoke = async id => {
    await stream.publish(start(id));
    assert(seen.has(id), 'tool card must exist before the handler runs');
    assert.equal(seen.get(id).finished, false);
    calls++;
    await stream.publish({tool_calls:[{...seen.get(id),finished:true,result:'labs result'}]});
  };
  const producer = Promise.all([invoke('first'), invoke('second')]).then(() => stream.finish());
  // Simulate the delegate forwarding and Theia registering each tool card.
  for await (const part of stream.consume()) {
    const call = part.tool_calls[0];
    if (call.finished) assert(seen.has(call.id));
    seen.set(call.id, call);
  }
  await producer;
  assert.equal(calls, 2);
  assert([...seen.values()].every(call => call.finished));
});

test('an awaiting confirmation does not block other tool cards or execute early', async () => {
  const stream = new ClaudeCodeResponseStream();
  let confirm;
  const confirmation = new Promise(resolve => { confirm = resolve; });
  let executed = false;
  const producer = (async () => {
    await stream.publish(start('confirmation'));
    await confirmation;
    executed = true;
    stream.finish();
  })();
  const iterator = stream.consume();
  assert.equal((await iterator.next()).value.tool_calls[0].id, 'confirmation');
  assert.equal(executed, false);
  const end = iterator.next();
  await Promise.resolve();
  assert.equal(executed, false);
  confirm();
  assert.equal((await end).done, true);
  await producer;
});

test('closing the consumer releases blocked publishers without invoking tools', async () => {
  const stream = new ClaudeCodeResponseStream();
  let invoked = false;
  const publication = stream.publish(start('cancelled')).then(() => { invoked = true; });
  const rejected = assert.rejects(publication, /closed/);
  const iterator = stream.consume();
  await iterator.next();
  await iterator.return();
  await rejected;
  assert.equal(invoked, false);
});

test('producer failures propagate and cancellation wakes an idle consumer', async () => {
  const stream = new ClaudeCodeResponseStream();
  const next = stream.consume().next();
  stream.finish(new Error('SDK failure'));
  await assert.rejects(next, /SDK failure/);
  const cancelled = new ClaudeCodeResponseStream();
  const pending = cancelled.consume().next();
  cancelled.dispose();
  assert.equal((await pending).done, true);
});
