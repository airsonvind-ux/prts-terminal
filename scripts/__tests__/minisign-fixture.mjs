import crypto from "node:crypto";

/**
 * Build a self-contained minisign fixture for tests.
 *
 * The fixture uses Node's Ed25519 implementation directly and emits the same
 * line structure Tauri's updater artifacts use:
 *   - a base64-encoded minisign public-key file in tauri.conf.json
 *   - a base64-encoded minisign signature file as <installer>.sig
 */
export function createMinisignFixture({
  version = "1.2.3",
  filename = "installer.exe",
  data = Buffer.from("installer bytes"),
  prehashed = true,
} = {}) {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const spki = publicKey.export({ format: "der", type: "spki" });
  const rawPublicKey = spki.subarray(spki.length - 32);
  const keyId = crypto.randomBytes(8);
  const algorithm = prehashed ? "ED" : "Ed";

  const publicKeyText = [
    `untrusted comment: minisign public key: ${keyId.toString("hex")}`,
    Buffer.concat([Buffer.from(algorithm, "ascii"), keyId, rawPublicKey]).toString("base64"),
    "",
  ].join("\n");
  const tauriPubkey = Buffer.from(publicKeyText, "utf8").toString("base64");

  const signedData = prehashed
    ? crypto.createHash("blake2b512").update(data).digest()
    : data;
  const signature = crypto.sign(null, signedData, privateKey);
  const trustedComment = `timestamp:1700000000\tfile:${filename}\tversion:${version}`;
  const globalSignature = crypto.sign(
    null,
    Buffer.concat([signature, Buffer.from(trustedComment, "utf8")]),
    privateKey,
  );

  const signatureText = [
    "untrusted comment: signature from test key",
    Buffer.concat([Buffer.from(algorithm, "ascii"), keyId, signature]).toString("base64"),
    `trusted comment: ${trustedComment}`,
    globalSignature.toString("base64"),
    "",
  ].join("\n");

  return {
    data,
    filename,
    version,
    keyId,
    tauriPubkey,
    publicKeyText,
    signatureText,
    signatureBase64: Buffer.from(signatureText, "utf8").toString("base64"),
    trustedComment,
  };
}
