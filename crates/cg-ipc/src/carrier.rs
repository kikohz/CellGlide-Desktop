//! Mobile carriers and US sub-brands, recognized from the carrier bundle the phone reports over USB
//! (and from the name its app sends). Same data and matching order as the Mac app's CarrierBadge.

use crate::phone::SimCarrier;
use cg_i18n::Lang;

#[derive(Debug, PartialEq, Eq)]
pub struct Carrier {
    pub name: &'static str,
    pub zh_name: Option<&'static str>,
    pub monogram: &'static str,
    pub zh_monogram: Option<&'static str>,
    /// 0xRRGGBB.
    pub color: u32,
    /// A bundled brand glyph, when one exists.
    pub icon: Option<&'static str>,
}

impl Carrier {
    pub fn display(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::ZhCn => self.zh_name.unwrap_or(self.name),
            Lang::En => self.name,
        }
    }

    pub fn display_monogram(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::ZhCn => self.zh_monogram.unwrap_or(self.monogram),
            Lang::En => self.monogram,
        }
    }
}

const fn plain(name: &'static str, monogram: &'static str, color: u32) -> Carrier {
    Carrier {
        name,
        zh_name: None,
        monogram,
        zh_monogram: None,
        color,
        icon: None,
    }
}

const fn with_icon(mut carrier: Carrier, icon: &'static str) -> Carrier {
    carrier.icon = Some(icon);
    carrier
}

const fn chinese(
    name: &'static str,
    zh_name: &'static str,
    monogram: &'static str,
    zh_monogram: &'static str,
    color: u32,
) -> Carrier {
    Carrier {
        name,
        zh_name: Some(zh_name),
        monogram,
        zh_monogram: Some(zh_monogram),
        color,
        icon: None,
    }
}

static CHINA_MOBILE: Carrier = chinese("China Mobile", "中国移动", "CM", "移", 0x0085D0);
static CHINA_UNICOM: Carrier = chinese("China Unicom", "中国联通", "CU", "联", 0xE60012);
static CHINA_TELECOM: Carrier = chinese("China Telecom", "中国电信", "CT", "电", 0x005BAC);
static VERIZON: Carrier = with_icon(plain("Verizon", "V", 0xCD040B), "verizon");
static ATT: Carrier = with_icon(plain("AT&T", "AT", 0x009FDB), "atandt");
static T_MOBILE: Carrier = plain("T-Mobile", "T", 0xE20074);

static CRICKET: Carrier = plain("Cricket", "C", 0x60A845);
static METRO: Carrier = plain("Metro", "M", 0x6D2077);
static VISIBLE: Carrier = plain("Visible", "V", 0x1800FF);
static MINT: Carrier = plain("Mint Mobile", "M", 0x3BA676);
static GOOGLE_FI: Carrier = with_icon(plain("Google Fi", "Fi", 0x4285F4), "google");
static US_MOBILE: Carrier = plain("US Mobile", "US", 0x1E64F0);
static XFINITY: Carrier = plain("Xfinity Mobile", "X", 0x6138F5);
static SPECTRUM: Carrier = plain("Spectrum Mobile", "S", 0x0099D8);
static BOOST: Carrier = plain("Boost Mobile", "B", 0xF7931E);
static STRAIGHT_TALK: Carrier = plain("Straight Talk", "ST", 0xD71920);
static CONSUMER_CELLULAR: Carrier = plain("Consumer Cellular", "CC", 0x00698C);

static CHINA_MOBILE_HK: Carrier = chinese("China Mobile HK", "中国移动香港", "CM", "移", 0x0085D0);
static CHINA_BROADNET: Carrier = chinese("China Broadnet", "中国广电", "CB", "广", 0x6B2C91);
static CHUNGHWA: Carrier = chinese("Chunghwa Telecom", "中华电信", "CHT", "华", 0x0A5BAA);
static TAIWAN_MOBILE: Carrier = chinese("Taiwan Mobile", "台湾大哥大", "TWM", "台", 0xF26722);
static FAREASTONE: Carrier = chinese("FarEasTone", "远传电信", "FET", "远", 0xE5003A);
static CSL: Carrier = plain("csl / HKT", "csl", 0x00A650);
static SMARTONE: Carrier = plain("SmarTone", "S", 0xE4007F);
static HK_3: Carrier = plain("3HK", "3", 0x6E6E73);
static DOCOMO: Carrier = plain("docomo", "d", 0xCC0033);
static KDDI: Carrier = plain("au (KDDI)", "au", 0xEB5505);
static SOFTBANK: Carrier = plain("SoftBank", "SB", 0x8C8C8C);
static RAKUTEN: Carrier = with_icon(plain("Rakuten Mobile", "R", 0xBF0000), "rakuten");
static SK_TELECOM: Carrier = plain("SK Telecom", "SK", 0xEA002C);
static KT: Carrier = plain("KT", "KT", 0xE5002B);
static LG_UPLUS: Carrier = plain("LG U+", "U+", 0xE6007E);
static SINGTEL: Carrier = plain("Singtel", "S", 0xE60000);
static STARHUB: Carrier = plain("StarHub", "SH", 0x3C9C35);
static TELSTRA: Carrier = plain("Telstra", "T", 0x0099F8);
static OPTUS: Carrier = plain("Optus", "O", 0x007C92);
static VODAFONE: Carrier = with_icon(plain("Vodafone", "V", 0xE60000), "vodafone");
static ORANGE: Carrier = with_icon(plain("Orange", "O", 0xFF7900), "orange");
static MOVISTAR: Carrier = with_icon(plain("Movistar", "M", 0x019DF4), "movistar");
static ROGERS: Carrier = plain("Rogers", "R", 0xDA291C);
static TELUS: Carrier = plain("TELUS", "T", 0x4B286D);

struct Entry {
    keywords: &'static [&'static str],
    carrier: &'static Carrier,
}

const fn entry(keywords: &'static [&'static str], carrier: &'static Carrier) -> Entry {
    Entry { keywords, carrier }
}

/// Checked in order: sub-brands before the networks whose names their bundles contain.
static BY_BUNDLE: &[Entry] = &[
    entry(&["_aio", "cricket"], &CRICKET),
    entry(&["metro"], &METRO),
    entry(&["visible"], &VISIBLE),
    entry(&["mint"], &MINT),
    entry(&["googlefi", "google_fi", "projectfi"], &GOOGLE_FI),
    entry(&["usmobile", "us_mobile"], &US_MOBILE),
    entry(&["xfinity", "comcast"], &XFINITY),
    entry(&["spectrum", "charter"], &SPECTRUM),
    entry(&["boost"], &BOOST),
    entry(
        &[
            "straighttalk",
            "tracfone",
            "totalwireless",
            "total_wireless",
        ],
        &STRAIGHT_TALK,
    ),
    entry(
        &["consumercellular", "consumer_cellular"],
        &CONSUMER_CELLULAR,
    ),
    entry(&["tmobile", "t-mobile"], &T_MOBILE),
    entry(&["verizon"], &VERIZON),
    entry(&["att"], &ATT),
    entry(&["cmcc", "chinamobile", "china_mobile"], &CHINA_MOBILE),
    entry(&["unicom", "cucc"], &CHINA_UNICOM),
    entry(&["chinatelecom", "china_telecom", "ctcc"], &CHINA_TELECOM),
];

/// Checked in order, so a regional subsidiary comes before its parent's keywords.
static BY_NAME: &[Entry] = &[
    entry(
        &["china mobile hong kong", "china mobile hk", "cmhk"],
        &CHINA_MOBILE_HK,
    ),
    entry(&["china mobile", "cmnet"], &CHINA_MOBILE),
    entry(&["china unicom", "china169", "unicom"], &CHINA_UNICOM),
    entry(&["chinanet", "china telecom"], &CHINA_TELECOM),
    entry(
        &["broadnet", "china broadcasting", "china broadcast"],
        &CHINA_BROADNET,
    ),
    entry(&["chunghwa"], &CHUNGHWA),
    entry(&["taiwan mobile"], &TAIWAN_MOBILE),
    entry(&["far eastone", "fareastone"], &FAREASTONE),
    entry(
        &["hong kong telecommunications", "hkt", "pccw", "csl"],
        &CSL,
    ),
    entry(&["smartone"], &SMARTONE),
    entry(&["hutchison", "three hk"], &HK_3),
    entry(&["verizon", "cellco"], &VERIZON),
    entry(&["at&t", "att mobility"], &ATT),
    entry(&["t-mobile", "tmobile"], &T_MOBILE),
    entry(&["docomo"], &DOCOMO),
    entry(&["kddi"], &KDDI),
    entry(&["softbank"], &SOFTBANK),
    entry(&["rakuten"], &RAKUTEN),
    entry(&["sk telecom"], &SK_TELECOM),
    entry(&["korea telecom", "kt corporation"], &KT),
    entry(&["lg uplus", "lg dacom", "lgu+"], &LG_UPLUS),
    entry(&["singtel"], &SINGTEL),
    entry(&["starhub"], &STARHUB),
    entry(&["telstra"], &TELSTRA),
    entry(&["optus"], &OPTUS),
    entry(&["vodafone"], &VODAFONE),
    entry(&["orange"], &ORANGE),
    entry(&["telefonica", "movistar"], &MOVISTAR),
    entry(&["rogers"], &ROGERS),
    entry(&["telus"], &TELUS),
];

/// Home networks by MCC + MNC, for bundles the catalog doesn't name.
fn by_network(code: &str) -> Option<&'static Carrier> {
    Some(match code {
        "310410" | "310150" | "310170" | "310280" | "310380" | "310560" | "310680" | "311180" => {
            &ATT
        }
        "311480" | "310004" | "310012" | "311270" | "311280" | "311390" | "310590" => &VERIZON,
        "310260" | "310160" | "310200" | "310210" | "310220" | "310230" | "310240" | "310250"
        | "310270" | "310310" | "310490" | "310660" | "310800" | "312250" => &T_MOBILE,
        "46000" | "46002" | "46004" | "46007" | "46008" => &CHINA_MOBILE,
        "46001" | "46006" | "46009" => &CHINA_UNICOM,
        "46003" | "46005" | "46011" => &CHINA_TELECOM,
        _ => return None,
    })
}

fn network(mcc: Option<&str>, mnc: Option<&str>) -> Option<&'static Carrier> {
    let (mcc, mnc) = (mcc?, mnc?);
    by_network(&format!("{mcc}{mnc}"))
}

/// From the carrier bundle (e.g. "com.apple.ATT_aio_NR_US"), which also names sub-brands;
/// the home network code (MCC + MNC) is the fallback.
pub fn match_bundle(
    bundle: Option<&str>,
    mcc: Option<&str>,
    mnc: Option<&str>,
) -> Option<&'static Carrier> {
    if let Some(bundle) = bundle {
        let id = bundle.to_lowercase().replace("com.apple.", "");
        if let Some(found) = BY_BUNDLE
            .iter()
            .find(|e| e.keywords.iter().any(|k| id.contains(k)))
        {
            return Some(found.carrier);
        }
    }
    network(mcc, mnc)
}

fn is_named(carrier: &Carrier, name: &str) -> bool {
    carrier.name == name || carrier.zh_name == Some(name)
}

/// Whether a SIM rides on the network named `name` (a Cricket SIM on "AT&T"), by its MCC + MNC.
fn rides(mcc: Option<&str>, mnc: Option<&str>, name: &str) -> bool {
    network(mcc, mnc).is_some_and(|c| is_named(c, name))
}

fn sim_carrier(sim: &SimCarrier) -> Option<&'static Carrier> {
    match_bundle(
        sim.bundle.as_deref(),
        sim.mcc.as_deref(),
        sim.mnc.as_deref(),
    )
}

/// The data line's carrier among the phone's SIMs. One SIM is the data line; with several,
/// the one on the network the phone is using (`in_use`, the name its app reports), or `None`
/// when that can't be told.
pub fn data_line(sims: &[SimCarrier], in_use: Option<&str>) -> Option<&'static Carrier> {
    let known: Vec<(&SimCarrier, &'static Carrier)> = sims
        .iter()
        .filter_map(|s| sim_carrier(s).map(|c| (s, c)))
        .collect();
    if known.len() == 1 && sims.len() == 1 {
        return Some(known[0].1);
    }
    let in_use = in_use?;
    let mut on_network = known.iter().filter(|(sim, carrier)| {
        rides(sim.mcc.as_deref(), sim.mnc.as_deref(), in_use) || is_named(carrier, in_use)
    });
    match (on_network.next(), on_network.next()) {
        (Some((_, carrier)), None) => Some(carrier),
        _ => None,
    }
}

/// The carrier for a name this catalog produced (the phone app sends its carrier's name).
pub fn match_name(name: &str) -> Option<&'static Carrier> {
    BY_BUNDLE
        .iter()
        .chain(BY_NAME)
        .find(|e| is_named(e.carrier, name))
        .map(|e| e.carrier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(bundle: &str, mcc: &str, mnc: &str) -> SimCarrier {
        SimCarrier {
            bundle: Some(bundle.into()),
            mcc: Some(mcc.into()),
            mnc: Some(mnc.into()),
        }
    }

    #[test]
    fn bundle_names_sub_brands_before_networks() {
        assert_eq!(
            match_bundle(Some("com.apple.ATT_aio_NR_US"), None, None)
                .unwrap()
                .name,
            "Cricket"
        );
        assert_eq!(
            match_bundle(Some("com.apple.ATT_US"), None, None)
                .unwrap()
                .name,
            "AT&T"
        );
        assert_eq!(
            match_bundle(Some("com.apple.Unknown"), Some("310"), Some("410"))
                .unwrap()
                .name,
            "AT&T"
        );
        assert_eq!(
            match_bundle(None, Some("460"), Some("00")).unwrap().zh_name,
            Some("中国移动")
        );
    }

    #[test]
    fn data_line_with_one_or_two_sims() {
        assert_eq!(
            data_line(&[sim("com.apple.ATT_aio_NR_US", "310", "410")], None)
                .unwrap()
                .name,
            "Cricket"
        );
        let two = [
            sim("com.apple.Verizon_US", "311", "480"),
            sim("com.apple.ATT_aio_NR_US", "310", "410"),
        ];
        assert!(data_line(&two, None).is_none());
        assert_eq!(data_line(&two, Some("AT&T")).unwrap().name, "Cricket");
        assert_eq!(match_name("中国联通").unwrap().name, "China Unicom");
    }

    #[test]
    fn display_follows_the_language() {
        let mobile = match_name("China Mobile").unwrap();
        assert_eq!(mobile.display(Lang::ZhCn), "中国移动");
        assert_eq!(mobile.display(Lang::En), "China Mobile");
        assert_eq!(match_name("AT&T").unwrap().display(Lang::ZhCn), "AT&T");
    }
}
