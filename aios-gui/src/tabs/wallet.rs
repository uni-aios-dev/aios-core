use crate::app::AiosApp;
use crate::theme::AiosTheme;
use crate::widgets::section::section;

/// Local HTML page used to verify the injected `window.ethereum` provider.
#[cfg(feature = "webview")]
pub const WEB3_TEST_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>AIOS Web3 Test</title>
<style>
body{background:#0b1018;color:#e8eef7;font:14px/1.5 system-ui,sans-serif;margin:0;padding:24px}
h1{font-size:20px;margin:0 0 6px}
button{background:#3d7dff;color:#fff;border:0;border-radius:6px;padding:8px 14px;cursor:pointer;margin:0 6px 12px 0}
pre{background:#111927;border:1px solid #2a3b55;border-radius:8px;padding:14px;white-space:pre-wrap;word-break:break-all}
small{color:#9fb3cc}
</style>
</head>
<body>
<h1>AIOS Web3 provider</h1>
<small>EIP-1193 bridge backed by the local AIOS wallet.</small><br><br>
<button id="connect">Connect</button>
<button id="sign">Sign message</button>
<button id="balance">Read balance</button>
<pre id="out">Ready.</pre>
<script>
const out = (line) => { document.getElementById('out').textContent += '\n' + line; };
const hex = (text) => Array.from(new TextEncoder().encode(text), b => b.toString(16).padStart(2,'0')).join('');
const provider = window.ethereum;
if (!provider) { out('ERROR: window.ethereum was not injected'); }
document.getElementById('connect').onclick = async () => {
  try {
    const accounts = await provider.request({method:'eth_requestAccounts'});
    out('accounts: ' + JSON.stringify(accounts));
    out('chainId: ' + await provider.request({method:'eth_chainId'}));
  } catch (e) { out('connect error ' + e.code + ': ' + e.message); }
};
document.getElementById('balance').onclick = async () => {
  try {
    const accounts = await provider.request({method:'eth_accounts'});
    const balance = await provider.request({method:'eth_getBalance', params:[accounts[0] || '0x0000000000000000000000000000000000000000','latest']});
    out('balance: ' + balance);
  } catch (e) { out('balance error ' + e.code + ': ' + e.message); }
};
document.getElementById('sign').onclick = async () => {
  try {
    const accounts = await provider.request({method:'eth_requestAccounts'});
    const signature = await provider.request({method:'personal_sign', params:['0x' + hex('Hello from AIOS'), accounts[0]]});
    out('signature: ' + signature);
  } catch (e) { out('sign error ' + e.code + ': ' + e.message); }
};
provider.on('accountsChanged', (a) => out('accountsChanged: ' + JSON.stringify(a)));
provider.on('chainChanged', (c) => out('chainChanged: ' + c));
</script>
</body>
</html>"#;

/// Wallet tab: account, network selection and provider verification.
pub fn show(ui: &mut egui::Ui, app: &mut AiosApp, theme: &AiosTheme) {
    let host = app.web3_host.clone();

    section(ui, theme, "Local Web3 Wallet", |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Address").color(theme.text_dim));
            ui.label(
                egui::RichText::new(host.address())
                    .color(theme.accent)
                    .monospace()
                    .size(12.0),
            );
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(host.address().to_string());
            }
        });
        ui.label(
            egui::RichText::new(format!("Selected network: {}", host.chain_id()))
                .color(theme.text)
                .size(12.0),
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for chain in host.chains() {
                let label = if chain.chain_id == host.chain_id() {
                    format!("\u{2713} {}", chain.name)
                } else {
                    format!("Use {}", chain.name)
                };
                if ui.button(label).clicked() {
                    host.select_chain(&chain.chain_id);
                }
            }
        });
    });

    ui.add_space(8.0);

    section(ui, theme, "Injected Provider", |ui| {
        ui.label(
            egui::RichText::new(
                "The native browser injects window.ethereum with eth_requestAccounts, \
                 eth_chainId, eth_getBalance, eth_call and personal_sign.",
            )
            .color(theme.text)
            .size(12.0),
        );
        ui.add_space(4.0);
        #[cfg(feature = "webview")]
        if ui.button("\u{1f9ea} Open Web3 test page").clicked() {
            if let Err(error) = app.open_web3_test_page() {
                app.browser_status = Some(error);
            }
        }
        #[cfg(not(feature = "webview"))]
        ui.label(
            egui::RichText::new("Native WebView feature is disabled in this build.")
                .color(theme.muted)
                .size(11.0),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Account access and signing require confirmation in the page dialog. \
                 The embedded text browser supports read-only RPC only.",
            )
            .color(theme.text_dim)
            .size(11.0),
        );
    });
}
