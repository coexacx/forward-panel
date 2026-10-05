use crate::{b, n, now, s, valid_id};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use md5::Md5;
use serde_json::{Value, json};
use sha2::Digest;
use spki::der::Decode;
use std::collections::{BTreeMap, HashSet};
use subtle::ConstantTimeEq;
pub struct Provider {
    config: Value,
    private: Option<ring::signature::RsaKeyPair>,
    public: Vec<u8>,
}
pub type Fields = BTreeMap<String, String>;
fn invalid() -> anyhow::Error {
    crate::store::Fault {
        code: 400,
        message: "invalid payment data",
    }
    .into()
}
pub fn canonical(v: &Fields, exclude_zero: bool) -> Result<String> {
    let mut parts = Vec::new();
    for (k, val) in v {
        if !valid_id(k) || val.len() > 4096 {
            return Err(invalid());
        }
        if k != "sign" && k != "sign_type" && !val.is_empty() && !(exclude_zero && val == "0") {
            parts.push(format!("{k}={val}"));
        }
    }
    Ok(parts.join("&"))
}
pub fn parse(query: &str) -> Result<Fields> {
    if query.len() > 16384 {
        return Err(invalid());
    }
    let mut map = Fields::new();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if map.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(invalid());
        }
    }
    if map.len() > 40 {
        return Err(invalid());
    }
    Ok(map)
}
fn pem_bytes(raw: &str) -> Result<Vec<u8>> {
    let raw = raw
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<String>();
    B64.decode(raw.split_whitespace().collect::<String>())
        .map_err(|_| invalid())
}
pub fn cents(raw: &str) -> Result<i64> {
    let p: Vec<_> = raw.split('.').collect();
    let whole = p[0];
    if p.len() > 2
        || whole.is_empty()
        || whole.len() > 7
        || whole.len() > 1 && whole.starts_with('0')
        || !whole.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(invalid());
    }
    let mut amount = whole.parse::<i64>()? * 100;
    if p.len() == 2 {
        if p[1].is_empty() || p[1].len() > 2 || !p[1].bytes().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }
        amount += p[1].parse::<i64>()? * if p[1].len() == 1 { 10 } else { 1 };
    }
    Ok(amount)
}
impl Provider {
    pub fn new(c: Value) -> Result<Option<Self>> {
        if !b(&c, "enabled") {
            return Ok(None);
        }
        for k in ["gateway", "public_origin"] {
            let u = url::Url::parse(s(&c, k)).map_err(|_| invalid())?;
            if u.scheme() != "https"
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err(invalid());
            }
        }
        let mid = s(&c, "merchant_id");
        if mid.is_empty() || mid.len() > 18 || !mid.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let methods = c["methods"].as_array().ok_or_else(invalid)?;
        if methods.is_empty() || methods.len() > 12 {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        let mut types = HashSet::new();
        for m in methods {
            let typ = s(m, "type");
            if !valid_id(s(m, "id"))
                || !ids.insert(s(m, "id"))
                || !types.insert(typ)
                || typ.is_empty()
                || typ.len() > 30
                || !typ.as_bytes()[0].is_ascii_lowercase()
                || !typ
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                || s(m, "name").is_empty()
                || s(m, "name").len() > 100
            {
                return Err(invalid());
            }
        }
        let mut p = Self {
            config: c,
            private: None,
            public: vec![],
        };
        match s(&p.config, "version") {
            "v1" => {
                if !(16..=256).contains(&s(&p.config, "secret").len()) {
                    return Err(invalid());
                }
            }
            "v2" => {
                let raw = pem_bytes(s(&p.config, "private_key"))?;
                let private = ring::signature::RsaKeyPair::from_pkcs8(&raw)
                    .or_else(|_| ring::signature::RsaKeyPair::from_der(&raw))
                    .map_err(|_| invalid())?;
                if private.public().modulus_len() < 256 {
                    return Err(invalid());
                }
                let raw = pem_bytes(s(&p.config, "platform_key"))?;
                let der = match spki::SubjectPublicKeyInfoRef::from_der(&raw) {
                    Ok(info) => info
                        .subject_public_key
                        .as_bytes()
                        .ok_or_else(invalid)?
                        .to_vec(),
                    Err(_) => raw,
                };
                let public = pkcs1::RsaPublicKey::from_der(&der).map_err(|_| invalid())?;
                if !(256..=1024).contains(&public.modulus.as_bytes().len()) {
                    return Err(invalid());
                }
                p.private = Some(private);
                p.public = der;
            }
            _ => return Err(invalid()),
        }
        Ok(Some(p))
    }
    fn sign(&self, v: &Fields) -> Result<String> {
        let c = canonical(v, b(&self.config, "exclude_zero"))?;
        if s(&self.config, "version") == "v1" {
            return Ok(hex::encode(Md5::digest(format!(
                "{c}{}",
                s(&self.config, "secret")
            ))));
        }
        let key = self.private.as_ref().ok_or_else(invalid)?;
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            c.as_bytes(),
            &mut signature,
        )
        .map_err(|_| invalid())?;
        Ok(B64.encode(signature))
    }
    pub fn checkout(&self, o: &Value, method: &str) -> Result<Value> {
        let cents = n(o, "amount_cents");
        let oid = s(o, "id");
        let title = s(&o["snapshot"], "name");
        if !valid_id(oid) || cents <= 0 || cents > 9999999 || title.len() > 127 {
            return Err(invalid());
        }
        let typ = self.config["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| s(m, "id") == method && b(m, "enabled"))
            .map(|m| s(m, "type"))
            .ok_or_else(invalid)?;
        let origin = s(&self.config, "public_origin").trim_end_matches('/');
        let mut v = Fields::from([
            ("pid".into(), s(&self.config, "merchant_id").into()),
            ("type".into(), typ.into()),
            ("out_trade_no".into(), oid.into()),
            ("name".into(), title.into()),
            (
                "money".into(),
                format!("{}.{:02}", cents / 100, cents % 100),
            ),
            (
                "notify_url".into(),
                format!("{origin}/control/payment/notify"),
            ),
            ("return_url".into(), format!("{origin}/app/orders")),
            ("sign_type".into(), "MD5".into()),
        ]);
        let path = if s(&self.config, "version") == "v2" {
            v.insert("sign_type".into(), "RSA".into());
            v.insert("timestamp".into(), now().to_string());
            "/api/pay/submit"
        } else {
            "/submit.php"
        };
        v.insert("sign".into(), self.sign(&v)?);
        let fields: BTreeMap<_, _> = v.into_iter().map(|(k, v)| (k, vec![v])).collect();
        Ok(
            json!({"action":format!("{}{path}",s(&self.config,"gateway").trim_end_matches('/')),"method":"POST","fields":fields}),
        )
    }
    pub fn verify(&self, v: &Fields) -> Result<(String, String, i64)> {
        let get = |key: &str| v.get(key).map(String::as_str).unwrap_or("");
        let canonical = canonical(v, b(&self.config, "exclude_zero"))?;
        if s(&self.config, "version") == "v1" {
            if !["", "MD5"].contains(&get("sign_type")) {
                return Err(invalid());
            }
            let signature = hex::decode(get("sign")).map_err(|_| invalid())?;
            let actual = Md5::digest(format!("{canonical}{}", s(&self.config, "secret")));
            if !bool::from(signature.as_slice().ct_eq(actual.as_slice())) {
                return Err(invalid());
            }
        } else {
            if get("sign_type") != "RSA" {
                return Err(invalid());
            }
            let sig = B64.decode(get("sign")).map_err(|_| invalid())?;
            ring::signature::UnparsedPublicKey::new(
                &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                &self.public,
            )
            .verify(canonical.as_bytes(), &sig)
            .map_err(|_| invalid())?;
            let t = get("timestamp").parse::<i64>().map_err(|_| invalid())?;
            if t < 1 || t > now() + 300 {
                return Err(invalid());
            }
        }
        if get("pid") != s(&self.config, "merchant_id")
            || get("trade_status") != "TRADE_SUCCESS"
            || !valid_id(get("out_trade_no"))
            || !valid_id(get("trade_no"))
            || !self.config["methods"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| s(m, "type") == get("type"))
        {
            return Err(invalid());
        }
        Ok((
            get("trade_no").into(),
            get("out_trade_no").into(),
            cents(get("money"))?,
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_sign_fields_and_amounts() {
        assert!(parse("pid=1&pid=2").is_err());
        for s in ["1e3", "-1", "01", "2.000", "NaN"] {
            assert!(cents(s).is_err())
        }
        assert_eq!(cents("10.01").unwrap(), 1001);
    }
}
