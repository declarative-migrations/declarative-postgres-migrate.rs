// Finite dialect-fencing abstraction for PostgreSQL and CockroachDB.
import assert from 'node:assert/strict';
const initial=d=>({source:d[0],target:d[1],compared:false,planned:false,applied:false,rejected:false});
const actions=['compare','plan','apply','reject'];
const key=s=>JSON.stringify(s);
const same=s=>s.source===s.target;
function step(s,a){
  if(a==='compare')return {...s,compared:true};
  if(a==='plan'&&s.compared&&same(s))return {...s,planned:true};
  if(a==='apply'&&s.planned&&same(s))return {...s,applied:true};
  if(a==='reject'&&s.compared&&!same(s))return {...s,rejected:true};
  return s;
}
function verify(successor){
  const seen=new Map(),queue=[];
  for(const x of ['pg','cr'])for(const y of ['pg','cr']){
    const s=initial([x,y]);seen.set(key(s),s);queue.push(s);
  }
  for(let i=0;i<queue.length;i++){
    const s=queue[i];
    assert.ok(!s.planned||(s.compared&&same(s)),`unsafe plan ${key(s)}`);
    assert.ok(!s.applied||(s.planned&&same(s)),`unsafe apply ${key(s)}`);
    for(const a of actions){
      const n=successor(s,a);
      if(!seen.has(key(n))){
        assert.ok(seen.size<128,'state bound overflow');
        seen.set(key(n),n);queue.push(n);
      }
    }
  }
  return [...seen.values()];
}
const states=verify(step);
assert.ok(states.length>=10,'vacuous graph');
assert.ok(states.some(s=>s.source==='pg'&&s.target==='pg'&&s.applied));
assert.ok(states.some(s=>s.source==='pg'&&s.target==='cr'&&s.rejected&&!s.applied));
assert.throws(()=>verify((s,a)=>a==='apply'?{...s,applied:true}:step(s,a)),/unsafe/);
console.log(`model=dialect_fence states=${states.length} negative_controls=1 status=PASS scope=abstract`);
