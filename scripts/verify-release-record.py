#!/usr/bin/env python3
"""Verify a Tapid release-record v1 detached signature before parsing it."""
import base64, hashlib, json, sys
from datetime import datetime, timezone

PUBLIC_KEY = base64.b64decode("eYPvN15Ah8ytHoBd2jY+36Wh/5g1kbqhDA9TL6wPRWc=")
KEY_ID = "release-key-2026-01"
SUBJECT = "tapid-release-v1"
SCHEMA = "tapid-release-v1-signature"
Q = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = (-121665 * pow(121666, Q - 2, Q)) % Q
I = pow(2, (Q - 1) // 4, Q)
B = (15112221349535400772501151409588531511454012693041857206046113283949847762202,
     46316835694926478169428394003475163141307993866256225615783033603165251855960)

def fail(message):
    print("tapid installer: " + message, file=sys.stderr)
    raise SystemExit(1)

def canonical(value):
    if isinstance(value, dict):
        return ("{" + ",".join(json.dumps(k, ensure_ascii=False, separators=(",", ":")) + ":" + canonical(value[k]).decode() for k in sorted(value)) + "}").encode()
    if isinstance(value, list): return ("[" + ",".join(canonical(x).decode() for x in value) + "]").encode()
    if isinstance(value, str): return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
    if value is None or isinstance(value, bool): return json.dumps(value, separators=(",", ":")).encode()
    if isinstance(value, int) and value >= 0: return str(value).encode()
    fail("invalid signature JSON number")

def add(a, b):
    x1,y1=a; x2,y2=b; t=D*x1*x2*y1*y2%Q
    return ((x1*y2+x2*y1)*pow(1+t,Q-2,Q)%Q,(y1*y2+x1*x2)*pow(1-t,Q-2,Q)%Q)

def mul(p, n):
    r=(0,1)
    while n:
        if n&1: r=add(r,p)
        p=add(p,p); n//=2
    return r

def point(raw):
    if len(raw)!=32: raise ValueError()
    v=int.from_bytes(raw,"little"); sign=v>>255; y=v&((1<<255)-1)
    if y>=Q: raise ValueError()
    x2=(y*y-1)*pow(D*y*y+1,Q-2,Q)%Q; x=pow(x2,(Q+3)//8,Q)
    if (x*x-x2)%Q: x=x*I%Q
    if (x*x-x2)%Q or (x==0 and sign): raise ValueError()
    return (Q-x if x&1 != sign else x),y

def verify(pub, sig, message):
    try:
        r=point(sig[:32]); a=point(pub); s=int.from_bytes(sig[32:],"little")
        if s>=L: return False
        h=int.from_bytes(hashlib.sha512(sig[:32]+pub+message).digest(),"little")%L
        return mul(B,s*8)==mul(add(r,mul(a,h)),8)
    except (ValueError, IndexError): return False

def main(record_path, sidecar_path):
    try:
        record=open(record_path,"rb").read(); envelope=json.load(open(sidecar_path,encoding="utf-8"))
    except Exception as exc: fail("cannot read release record signature: " + str(exc))
    if not isinstance(envelope,dict) or set(envelope) != {"artifact_digest","claims","signature","subject","version"}: fail("invalid release record signature sidecar")
    claims=envelope.get("claims"); sig=envelope.get("signature")
    if envelope["version"] != "tapid-trust-envelope-v1" or envelope["subject"] != SUBJECT or not isinstance(claims,dict) or not isinstance(sig,dict): fail("invalid release signature identity")
    if envelope["artifact_digest"] != "sha256-"+hashlib.sha256(record).hexdigest() or sig.get("artifact_digest") != envelope["artifact_digest"]: fail("release record signature digest mismatch")
    if claims.get("schema") != SCHEMA or sig.get("algorithm") != "ed25519" or sig.get("key_id") != KEY_ID or sig.get("subject") != SUBJECT: fail("invalid release signature claims")
    try:
        created=datetime.fromisoformat(claims["created_at"].replace("Z","+00:00")); expires=datetime.fromisoformat(claims["expires_at"].replace("Z","+00:00")); now=datetime.now(timezone.utc)
    except Exception: fail("invalid release signature timestamps")
    if created > now or expires <= now or expires <= created or (expires-created).days > 30: fail("release record signature is expired or has an invalid validity window")
    try: value=base64.b64decode(sig["value"],validate=True)
    except Exception: fail("invalid release signature encoding")
    payload={"artifact_digest":envelope["artifact_digest"],"claims":claims,"subject":SUBJECT,"version":envelope["version"],"signature_context":{"algorithm":"ed25519","key_id":KEY_ID}}
    if len(value)!=64 or not verify(PUBLIC_KEY,value,canonical(payload)): fail("release record signature verification failed")

if __name__ == "__main__":
    if len(sys.argv)!=3: fail("usage: verify-release-record.py RECORD SIDECAR")
    main(sys.argv[1],sys.argv[2])
