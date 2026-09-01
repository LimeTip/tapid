import assert from "node:assert/strict";
import isNumber from "is-number";

assert.equal(isNumber("42"), true, "installed package should recognize a number");
assert.equal(isNumber("tapid"), false, "installed package should reject non-numeric text");

console.log("public consumer dependency executed successfully");
