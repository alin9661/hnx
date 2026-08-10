# Security policy

Please report vulnerabilities privately through GitHub's security advisory
form rather than a public issue.

`hnx` treats all network text as untrusted. Terminal control sequences are
removed before display, external URLs are limited to HTTP(S), and article
downloads are bounded by content type, redirect count, deadline, and response
size. Article DNS is validated inside the HTTP connector; non-public address
answers and proxy-side re-resolution are rejected to prevent SSRF and DNS
rebinding. API JSON and comment traversal also have response, concurrency,
node, depth, cycle, and overall-time limits. Reports that bypass those
boundaries are especially useful.
