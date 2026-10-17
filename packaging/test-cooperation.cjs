// Only synthetic config and a generated script in a disposable VM. Never a client profile.
const assert = require('node:assert/strict');
const {execFileSync} = require('node:child_process');
const vm = require('node:vm');
const path = require('node:path');
const source = JSON.parse(execFileSync(path.resolve('target/debug/examples/cooperation_fixture.exe'),
  [], {encoding:'utf8',windowsHide:true})).script;
function apply(config) {
  const context = vm.createContext({input:structuredClone(config)});
  vm.runInContext(source,context,{timeout:1000});
  return JSON.parse(vm.runInContext('JSON.stringify(main(input))',context,{timeout:1000}));
}
const original = {
  mode:'rule','mixed-port':7897,
  proxies:[{name:'user-node',type:'http',server:'127.0.0.1',port:7890}],
  'proxy-groups':[{name:'user-choice',type:'select',proxies:['user-node']}],
  rules:['DOMAIN-SUFFIX,arena.ai,user-choice','MATCH,user-choice'],
  dns:{enable:true,nameserver:['127.0.0.1:9'],'nameserver-policy':{'example.org':['127.0.0.1:9']}},
  'client-owned':{value:'preserve-exactly'},
};
const candidate = apply(original);
assert.deepEqual(candidate['proxy-groups'], original['proxy-groups']);
assert.deepEqual(candidate['client-owned'], original['client-owned']);
assert.deepEqual(candidate.proxies[0],original.proxies[0]);
assert.deepEqual(candidate.rules.slice(-2),original.rules);
assert.deepEqual(candidate.dns.nameserver,original.dns.nameserver);
assert.deepEqual(candidate.dns['nameserver-policy']['example.org'],original.dns['nameserver-policy']['example.org']);
assert.deepEqual(apply(candidate),candidate,'the last subscription script must be idempotent');
assert.equal(original.proxies.length,1,'preview never mutates its input');
assert.ok(candidate.rules[0].includes('NETWORK,UDP'));
assert.ok(candidate.rules[0].endsWith(',REJECT'));
assert.equal(candidate.proxies.at(-1).port,17328);
assert.ok(candidate.dns['nameserver-policy']['+.claude.ai'][0].endsWith('#AEC-MANAGED-FIXED'));
for(const mode of ['global','direct']) assert.throws(()=>apply({...original,mode}),/COOP_CLIENT_NOT_RULE_MODE/);
assert.throws(()=>apply({...original,'mixed-port':17328}),/COOP_PROXY_LOOP/);
assert.throws(()=>apply({...original,proxies:[{name:'AEC-MANAGED-FIXED',type:'direct'}]}),/COOP_OWNERSHIP_CONFLICT/);
const conflicting=structuredClone(original);
conflicting.dns['nameserver-policy']['+.claude.ai']=['127.0.0.1:9'];
assert.throws(()=>apply(conflicting),/COOP_DNS_POLICY_CONFLICT/);
console.log('PASS: scoped subscription script preserves user configuration, is idempotent, rejects conflicts, loops and non-rule modes; no production IO.');
