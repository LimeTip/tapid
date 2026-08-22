# tapid-attestations

Typed artifact-bound claims covering issuer, methodology, scope, issue/expiry, findings, confidence, limitations, and payment disclosure. Claims require canonical RFC 3339 timestamps with an explicit `Z` or numeric offset; issue and expiry are compared as instants before conversion to an unsigned canonical trust envelope. The unsigned envelope remains a transport/canonicalization primitive and is not evidence of signature verification or trust.
