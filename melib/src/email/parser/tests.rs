//
// melib
//
// Copyright 2017 Emmanouil Pitsidianakis <manos@pitsidianak.is>
//
// This file is part of meli.
//
// meli is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// meli is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with meli. If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: EUPL-1.2 OR GPL-3.0-or-later

use crate::{
    email::{
        address::*,
        headers::{HeaderMap, HeaderName},
        mailto::Mailto,
        parser::{
            address::*,
            dates::rfc5322_date,
            encodings::*,
            generic::{comment, phrase2, unstructured},
            headers,
            mailing_lists::rfc_2369_list_headers_action_list,
            BytesExt,
        },
    },
    make_address,
};

macro_rules! to_str {
    ($l:expr) => {{
        unsafe { std::str::from_utf8_unchecked($l) }
    }};
}

#[test]
fn test_email_parser_phrase() {
    let words = b"=?iso-8859-7?B?W215Y291cnNlcy5udHVhLmdyIC0gyvXs4fTp6t4g6uHpIMri4e306ere?=
     =?iso-8859-7?B?INb18+nq3l0gzd3hIMHt4erv3+358+c6IMzF0c/TIMHQz9TFy8XTzMHU?=
      =?iso-8859-7?B?2c0gwiDUzC4gysHNLiDFzsXUwdPH0yAyMDE3LTE4OiDTx8zFydnTxw==?=";
    assert_eq!(
        "[mycourses.ntua.gr - Κυματική και Κβαντική Φυσική] Νέα Ανακοίνωση: ΜΕΡΟΣ ΑΠΟΤΕΛΕΣΜΑΤΩΝ Β \
         ΤΜ. ΚΑΝ. ΕΞΕΤΑΣΗΣ 2017-18: ΣΗΜΕΙΩΣΗ",
        std::str::from_utf8(&phrase(words.trim(), false).unwrap().1).unwrap()
    );
    let words = b"=?UTF-8?Q?=CE=A0=CF=81=CF=8C=CF=83=CE=B8=CE=B5?= =?UTF-8?Q?=CF=84=CE=B7_=CE=B5=CE=BE=CE=B5=CF=84?= =?UTF-8?Q?=CE=B1=CF=83=CF=84=CE=B9=CE=BA=CE=AE?=";
    assert_eq!(
        "Πρόσθετη εξεταστική",
        std::str::from_utf8(&phrase(words.trim(), false).unwrap().1).unwrap()
    );
    let words = b"[Advcomparch] =?utf-8?b?zqPPhc68z4DOtc+BzrnPhs6/z4HOrCDPg861IGZs?=\n\t=?utf-8?b?dXNoIM67z4zOs8+JIG1pc3ByZWRpY3Rpb24gzrrOsc+Ezqwgz4TOt869?=\n\t=?utf-8?b?IM61zrrPhM6tzrvOtc+Dzrcgc3RvcmU=?=";
    assert_eq!(
        "[Advcomparch] Συμπεριφορά σε flush λόγω misprediction κατά την εκτέλεση store",
        std::str::from_utf8(&phrase(words.trim(), false).unwrap().1).unwrap()
    );
    let words = b"Re: [Advcomparch] =?utf-8?b?zqPPhc68z4DOtc+BzrnPhs6/z4HOrCDPg861IGZs?=
	=?utf-8?b?dXNoIM67z4zOs8+JIG1pc3ByZWRpY3Rpb24gzrrOsc+Ezqwgz4TOt869?=
	=?utf-8?b?IM61zrrPhM6tzrvOtc+Dzrcgc3RvcmU=?=";
    assert_eq!(
        "Re: [Advcomparch] Συμπεριφορά σε flush λόγω misprediction κατά την εκτέλεση store",
        std::str::from_utf8(&phrase(words.trim(), false).unwrap().1).unwrap()
    );
    let words = b"sdf";
    assert_eq!(
        "sdf",
        std::str::from_utf8(&phrase(words, false).unwrap().1).unwrap()
    );
    let words = b"=?iso-8859-7?b?U2VnIGZhdWx0IPP05+0g5er03evl8+cg9O/1?= =?iso-8859-7?q?_example_ru_n_=5Fsniper?=";
    assert_eq!(
        "Seg fault στην εκτέλεση του example ru n _sniper",
        std::str::from_utf8(&phrase(words, false).unwrap().1).unwrap()
    );
    let words = b"Re: [Advcomparch]
 =?iso-8859-7?b?U2VnIGZhdWx0IPP05+0g5er03evl8+cg9O/1?=
 =?iso-8859-7?q?_example_ru_n_=5Fsniper?=";

    assert_eq!(
        "Re: [Advcomparch] Seg fault στην εκτέλεση του example ru n _sniper",
        std::str::from_utf8(&phrase(words, false).unwrap().1).unwrap()
    );

    let words = r#"[internal] =?UTF-8?B?zp3Orc6/z4Igzp/OtM63zrPPjM+CIM6jz4XOs86zz4E=?=
 =?UTF-8?B?zrHPhs6uz4I=?="#;
    assert_eq!(
        "[internal] Νέος Οδηγός Συγγραφής",
        std::str::from_utf8(&phrase(words.as_bytes(), false).unwrap().1).unwrap()
    );

    let words = r#"=?UTF-8?Q?Re=3a_Climate_crisis_reality_check_=e2=80=93=c2=a0EcoHust?=
 =?UTF-8?Q?ler?="#;
    assert_eq!(
        "Re: Climate crisis reality check –\u{a0}EcoHustler",
        std::str::from_utf8(&phrase(words.as_bytes(), false).unwrap().1).unwrap()
    );

    let words = r#"Re: Climate crisis reality check =?windows-1250?B?lqBFY29IdXN0?=
 =?windows-1250?B?bGVy?="#;
    assert_eq!(
        "Re: Climate crisis reality check –\u{a0}EcoHustler",
        std::str::from_utf8(&phrase(words.as_bytes(), false).unwrap().1).unwrap()
    );

    let words = r#"=?gb18030?B?zNrRtsbz0rXTys/k19S2r9eqt6LR6dak08q8/g==?="#;
    assert_eq!(
        "腾讯企业邮箱自动转发验证邮件",
        std::str::from_utf8(&phrase(words.as_bytes(), false).unwrap().1).unwrap()
    );
}

#[test]
fn test_email_parser_address_list() {
    let s = b"Obit Oppidum <user@domain>,
            list <list@domain.tld>, list2 <list2@domain.tld>,
            Bobit Boppidum <user@otherdomain.com>, Cobit Coppidum <user2@otherdomain.com>, <user@domain.tld>";
    assert_eq!(
        (
            &s[0..0],
            smallvec::smallvec![
                make_address!("Obit Oppidum", "user@domain"),
                make_address!("list", "list@domain.tld"),
                make_address!("list2", "list2@domain.tld"),
                make_address!("Bobit Boppidum", "user@otherdomain.com"),
                make_address!("Cobit Coppidum", "user2@otherdomain.com"),
                make_address!("", "user@domain.tld")
            ]
        ),
        rfc2822address_list(s).unwrap()
    );
}

//    // [ref:FIXME]: add file
//#[test]
//fn test_email_parser_attachments() {
//            use std::io::Read;
//            let mut buffer: Vec<u8> = Vec::new();
//            let _ = std::fs::File::open("").unwrap().read_to_end(&mut buffer);
//            let boundary = b"b1_4382d284f0c601a737bb32aaeda53160";
//            let (_, body) = match mail(&buffer) {
//                Ok(v) => v,
//                Err(_) => panic!(),
//            };
//            let attachments = parts(body, boundary).unwrap().1;
//            assert_eq!(attachments.len(), 4);
//            let v: Vec<&str> = attachments
//                .iter()
//                .map(|v| std::str::from_utf8(v).unwrap())
//                .collect();
//    //println!("attachments {:?}", v);
//}

#[test]
fn test_email_parser_addresses() {
    macro_rules! assert_parse {
        ($name:literal, $addr:literal, $raw:literal) => {{
            #[allow(clippy::string_lit_as_bytes)]
            let s = $raw.as_bytes();
            let r = address(s).unwrap().1;
            match r {
                Address::Mailbox(ref m) => {
                    assert_eq!(to_str!(m.display_name.display_bytes(&m.raw)), $name);
                    assert_eq!(to_str!(m.address_spec.display_bytes(&m.raw)), $addr);
                }
                _ => assert!(false),
            }
        }};
    }

    assert_parse!(
        "Σταύρος Μαλτέζος",
        "maltezos@central.ntua.gr",
        "=?iso-8859-7?B?0/Th/fHv8iDM4ev03ebv8g==?= <maltezos@central.ntua.gr>"
    );
    assert_parse!("", "user@domain", "user@domain");
    assert_parse!("", "user@domain", "<user@domain>");
    assert_parse!("", "user@domain", "  <user@domain>");
    assert_parse!("Name", "user@domain", "Name <user@domain>");
    assert_parse!(
        "",
        "julia@ficdep.minitrue",
        "julia(outer party)@ficdep.minitrue"
    );
    assert_parse!(
        "Winston Smith",
        "winston.smith@recdep.minitrue",
        "\"Winston Smith\" <winston.smith@recdep.minitrue> (Records Department)"
    );
    assert_parse!(
        "John Q. Public",
        "JQB@bar.com",
        "\"John Q. Public\" <JQB@bar.com>"
    );
    assert_parse!(
        "John Q. Public",
        "JQB@bar.com",
        "John \"Q.\" Public <JQB@bar.com>"
    );
    assert_parse!(
        "John Q. Public",
        "JQB@bar.com",
        "\"John Q.\" Public <JQB@bar.com>"
    );
    assert_parse!(
        "John Q. Public",
        "JQB@bar.com",
        "John \"Q. Public\" <JQB@bar.com>"
    );
    assert_parse!(
        "Jeffrey Stedfast",
        "fejj@helixcode.com",
        "Jeffrey Stedfast <fejj@helixcode.com>"
    );
    assert_parse!(
        "this is\ta folded name",
        "folded@name.com",
        "this is\n\ta folded name <folded@name.com>"
    );
    assert_parse!(
        "Jeffrey fejj Stedfast",
        "fejj@helixcode.com",
        "Jeffrey fejj Stedfast <fejj@helixcode.com>"
    );
    assert_parse!(
        "Jeffrey fejj Stedfast",
        "fejj@helixcode.com",
        "Jeffrey \"fejj\" Stedfast <fejj@helixcode.com>"
    );
    assert_parse!(
        "Jeffrey \"fejj\" Stedfast",
        "fejj@helixcode.com",
        "\"Jeffrey \\\"fejj\\\" Stedfast\" <fejj@helixcode.com>"
    );
    assert_parse!(
        "Stedfast, Jeffrey",
        "fejj@helixcode.com",
        "\"Stedfast, Jeffrey\" <fejj@helixcode.com>"
    );
    assert_parse!(
        "",
        "fejj@helixcode.com",
        "fejj@helixcode.com (Jeffrey Stedfast)"
    );
    assert_parse!(
        "Jeffrey Stedfast",
        "fejj@helixcode.com",
        "Jeffrey Stedfast <fejj(nonrecursive block)@helixcode.(and a comment here)com>"
    );
    assert_parse!(
        "Jeffrey Stedfast",
        "fejj@helixcode.com",
        "Jeffrey Stedfast <fejj(recursive (comment) block)@helixcode.(and a comment here)com>"
    );
    assert_parse!(
        "Joe Q. Public",
        "john.q.public@example.com",
        "\"Joe Q. Public\" <john.q.public@example.com>"
    );
    assert_parse!("Mary Smith", "mary@x.test", "Mary Smith <mary@x.test>");
    assert_parse!("Mary Smith", "mary@x.test", "Mary Smith <mary@x.test>");
    assert_parse!("", "jdoe@example.org", "jdoe@example.org");
    assert_parse!("Who?", "one@y.test", "Who? <one@y.test>");
    assert_parse!("", "boss@nil.test", "<boss@nil.test>");
    assert_parse!(
        "Giant; \"Big\" Box",
        "sysservices@example.net",
        r#""Giant; \"Big\" Box" <sysservices@example.net>"#
    );
    //assert_eq!(
    //    make_address!("Jeffrey Stedfast", "fejj@helixcode.com"),
    //    address(b"Jeffrey Stedfast <fejj@helixcode.com.>")
    //        .unwrap()
    //        .1
    //);
    assert_parse!(
        "John <middle> Doe",
        "jdoe@machine.example",
        "\"John <middle> Doe\" <jdoe@machine.example>"
    );
    // RFC 2047 "Q"-encoded ISO-8859-1 address.
    assert_parse!(
        "Jörg Doe",
        "joerg@example.com",
        "=?iso-8859-1?q?J=F6rg_Doe?= <joerg@example.com>"
    );

    // RFC 2047 "Q"-encoded US-ASCII address. Dumb but legal.
    assert_parse!(
        "Jorg Doe",
        "joerg@example.com",
        "=?us-ascii?q?J=6Frg_Doe?= <joerg@example.com>"
    );
    // RFC 2047 "Q"-encoded UTF-8 address.
    assert_parse!(
        "Jörg Doe",
        "joerg@example.com",
        "=?utf-8?q?J=C3=B6rg_Doe?= <joerg@example.com>"
    );
    // RFC 2047 "Q"-encoded UTF-8 address with multiple encoded-words.
    assert_parse!(
        "JörgDoe",
        "joerg@example.com",
        "=?utf-8?q?J=C3=B6rg?=  =?utf-8?q?Doe?= <joerg@example.com>"
    );
    assert_parse!(
        "André Pirard",
        "PIRARD@vm1.ulg.ac.be",
        "=?ISO-8859-1?Q?Andr=E9?= Pirard <PIRARD@vm1.ulg.ac.be>"
    );
    // Custom example of RFC 2047 "B"-encoded ISO-8859-1 address.
    assert_parse!(
        "Jörg",
        "joerg@example.com",
        "=?ISO-8859-1?B?SvZyZw==?= <joerg@example.com>"
    );
    // Custom example of RFC 2047 "B"-encoded UTF-8 address.
    assert_parse!(
        "Jörg",
        "joerg@example.com",
        "=?UTF-8?B?SsO2cmc=?= <joerg@example.com>"
    );
    // Custom example with "." in name. For issue 4938
    //assert_parse!(
    //    "Asem H.",
    //    "noreply@example.com",
    //    "Asem H. <noreply@example.com>"
    //);
    assert_parse!(
        // RFC 6532 3.2.3, qtext /= UTF8-non-ascii
        "Gø Pher",
        "gopher@example.com",
        "\"Gø Pher\" <gopher@example.com>"
    );
    // RFC 6532 3.2, atext /= UTF8-non-ascii
    assert_parse!("µ", "micro@example.com", "µ <micro@example.com>");
    // RFC 6532 3.2.2, local address parts allow UTF-8
    //assert_parse!("Micro", "µ@example.com", "Micro <µ@example.com>");
    // RFC 6532 3.2.4, domains parts allow UTF-8
    //assert_parse!(
    //    "Micro",
    //    "micro@µ.example.com",
    //    "Micro <micro@µ.example.com>"
    //);
    // Issue 14866
    assert_parse!(
        "",
        "emptystring@example.com",
        "\"\" <emptystring@example.com>"
    );
    // CFWS
    assert_parse!(
        "",
        "cfws@example.com",
        "<cfws@example.com> (CFWS (cfws))  (another comment)"
    );
    //"<cfws@example.com> ()  (another comment), <cfws2@example.com> (another)"
    assert_parse!(
        "Kristoffer Brånemyr",
        "ztion@swipenet.se",
        "=?iso-8859-1?q?Kristoffer_Br=E5nemyr?= <ztion@swipenet.se>"
    );
    assert_parse!(
        "François Pons",
        "fpons@mandrakesoft.com",
        "=?iso-8859-1?q?Fran=E7ois?= Pons <fpons@mandrakesoft.com>"
    );
    assert_parse!(
        "هل تتكلم اللغة الإنجليزية /العربية؟",
        "do.you.speak@arabic.com",
        "=?utf-8?b?2YfZhCDYqtiq2YPZhNmFINin2YTZhNi62Kkg2KfZhNil2YbYrNmE2YrYstmK2Kk=?=\n \
         =?utf-8?b?IC/Yp9mE2LnYsdio2YrYqdif?= <do.you.speak@arabic.com>"
    );
    assert_parse!(
        "狂ったこの世で狂うなら気は確かだ。",
        "famous@quotes.ja",
        "=?utf-8?b?54uC44Gj44Gf44GT44Gu5LiW44Gn54uC44GG44Gq44KJ5rCX44Gv56K644GL44Gg?=\n \
         =?utf-8?b?44CC?= <famous@quotes.ja>"
    );
    assert_eq!(
        Address::new_group(
            "A Group".to_string(),
            vec![
                make_address!("Ed Jones", "c@a.test"),
                make_address!("", "joe@where.test"),
                make_address!("John", "jdoe@one.test")
            ]
        ),
        address(b"A Group:Ed Jones <c@a.test>,joe@where.test,John <jdoe@one.test>;")
            .unwrap()
            .1
    );
    assert_eq!(
        Address::new_group("Undisclosed recipients".to_string(), vec![]),
        address(b"Undisclosed recipients:;").unwrap().1
    );
    assert_parse!(
        "狂ったこの世で狂うなら気は確かだ。",
        "famous@quotes.ja",
        "狂ったこの世で狂うなら気は確かだ。 <famous@quotes.ja>"
    );
}

#[test]
fn test_email_parser_quoted_printable() {
    let input = r#"<=21-- SEPARATOR  -->
   <tr>
    <td style=3D=22padding-left: 10px;padding-right: 10px;background-color:=
 =23f3f5fa;=22>
     <table width=3D=22100%=22 cellspacing=3D=220=22 cellpadding=3D=220=22 =
border=3D=220=22>
      <tr>
       <td style=3D=22height:5px;background-color: =23f3f5fa;=22>&nbsp;</td>
      </tr>
     </table>
    </td>
   </tr>"#;
    assert_eq!(
        quoted_printable_bytes(input.as_bytes())
            .as_ref()
            .map(|(_, b)| unsafe { std::str::from_utf8_unchecked(b) }),
        Ok(r#"<!-- SEPARATOR  -->
   <tr>
    <td style="padding-left: 10px;padding-right: 10px;background-color: #f3f5fa;">
     <table width="100%" cellspacing="0" cellpadding="0" border="0">
      <tr>
       <td style="height:5px;background-color: #f3f5fa;">&nbsp;</td>
      </tr>
     </table>
    </td>
   </tr>"#)
    );
}

#[test]
fn test_email_parser_msg_id() {
    let s = "Message-ID: <1234@local.machine.example>\r\n";
    let (rest, (_header_name, value)) = headers::header(s.as_bytes()).unwrap();
    assert!(rest.is_empty());
    let a = msg_id(value).unwrap().1;
    assert_eq!(a.val(), b"<1234@local.machine.example>");
    let s = "Message-ID:              <testabcd.1234@silly.test>\r\n";
    let (rest, (_header_name, value)) = headers::header(s.as_bytes()).unwrap();
    assert!(rest.is_empty());
    let b = msg_id(value).unwrap().1;
    assert_eq!(b.val(), b"<testabcd.1234@silly.test>");
    let s = "References: <1234@local.machine.example>\r\n";
    let (rest, (_header_name, value)) = headers::header(s.as_bytes()).unwrap();
    assert!(rest.is_empty());
    assert_eq!(&msg_id_list(value).unwrap().1, &[a.clone()]);
    let s = "References: <1234@local.machine.example> <3456@example.net>\r\n";
    let (rest, (_header_name, value)) = headers::header(s.as_bytes()).unwrap();
    assert!(rest.is_empty());
    let s = b"<3456@example.net>";
    let c = msg_id(s).unwrap().1;
    assert_eq!(&msg_id_list(value).unwrap().1, &[a, c]);
}

#[rustfmt::skip]
    #[test]
    fn test_email_parser_mailto_parse() {
        let value = "mailto:1001084@bugs.debian.org?In-Reply-To=%3C168435582154.540248.\
             1403466294276093439%40xxxxxxxxxxxxxx%3E&References=%3C163857393129.1083042.\
             11053317018847169002.reportbug%40xxxxxxxxxxxxxx%3E%0A%20%3C168435582154.540248.\
             1403466294276093439%40xxxxxxxxxxxxxx%3E&subject=Re%3A%20ITP%3A%20meli%20--%\
             20terminal%20mail%20client&body=On%20Wed%2C%2017%20May%202023%2022%3A37%3A01%20%\
             2B0200%20xxxxx%20zzzzzzzzzz%20%3Cdr%40xxxxxxxx%3E%20wrote%3A%0A%3E%200.7.2%\
             2B20230517%20draft%201%20needs%20embedding%208%20crates%20%286%20missing%2C%201%\
             20unwanted%2C%201%20ahead%29%3B%0A%3E%20runs%20and%20seems%20to%20work%20from%20a%\
             20brief%20test%20use.%0A%3E%20%0A%3E%20Main%20tasks%20are%20still%20to%20keep%\
             20package%20up-to-date%20with%20upstream%20releases%2C%20and%0A%3E%20to%20package%\
             20more%20of%20the%20crates%20currently%20embedded.%0A%3E%20%0A%3E%20Here%27s%20how%\
             20you%20can%20help%3A%0A%3E%20%0A%3E%20As%20user%20running%20Debian%2C%20you%20can%\
             20test%20this%20draft%20package%3A%20Either%20build%20it%0A%3E%20yourself%20from%\
             20source%20or%20tell%20%28by%20posting%20to%20this%20bugreport%29%20if%20you%0A%3E%\
             20prefer%20testing%20the%20binary%20packages%20I%20built%20-%20then%20I%20will%\
             20share%20those.%0A%3E%20%0A%3E%20As%20developer%20%28but%20no%20need%20to%20be%\
             20official%20member%20of%20Debian%21%29%2C%20you%20can%0A%3E%20join%20the%20Debian%\
             20Rust%20team%20and%20help%20package%20these%20missing%20crates%3A%0A%3E%20https%3A%\
             2F%2Fsalsa.debian.org%2Fdebian%2Fmeli%2F-%2Fblob%2Fdebian%2Flatest%2Fdebian%2FTODO%\
             0A%3E%20%0A%3E%20%0A%3E%20%20-%20JJJJJ%0A%3E%20%0A%3E%20--%20%0A%3E%20%20%2A%20JJJJJ%\
             20xxxxxxxxxx%20-%20idealist%20%26%20Internet-arkitekt%0A%3E%20%20%2A%20Tlf.%3A%20%\
             2B45%20%20%20Website%3A%20http%3A%2F%2Fxxxxxxxxxxx%2F%0A%3E%20%0A%3E%20%20%5Bx%5D%\
             20quote%20me%20freely%20%20%5B%20%5D%20ask%20before%20reusing%20%20%5B%20%5D%20keep%\
             20private";
        let (rest, mailto) = crate::parser::generic::mailto(value.as_bytes())
            .map_err(|err| match err {
                nom::Err::Failure(err) | nom::Err::Error(err) => {
                    format!(
                        "kind {:?} at: {:?}",
                        err,
                        String::from_utf8_lossy(err.input)
                    )
                }
                nom::Err::Incomplete(_) => "incomplete input".to_string(),
            })
            .unwrap();
        println!(
            "rest = {}, mailto = {:#?}",
            String::from_utf8_lossy(rest),
            mailto
        );

        assert_eq!(
            mailto,
            Mailto {
                address: vec![
                    Address::new(None, "1001084@bugs.debian.org".to_string())

                ],
                body: Some(
                    "On Wed, 17 May 2023 22:37:01 +0200 xxxxx zzzzzzzzzz <dr@xxxxxxxx> wrote:\n> 0.7.2+20230517 draft 1 needs embedding 8 crates (6 missing, 1 unwanted, 1 ahead);\n> runs and seems to work from a brief test use.\n> \n> Main tasks are still to keep package up-to-date with upstream releases, and\n> to package more of the crates currently embedded.\n> \n> Here's how you can help:\n> \n> As user running Debian, you can test this draft package: Either build it\n> yourself from source or tell (by posting to this bugreport) if you\n> prefer testing the binary packages I built - then I will share those.\n> \n> As developer (but no need to be official member of Debian!), you can\n> join the Debian Rust team and help package these missing crates:\n> https://salsa.debian.org/debian/meli/-/blob/debian/latest/debian/TODO\n> \n> \n>  - JJJJJ\n> \n> -- \n>  * JJJJJ xxxxxxxxxx - idealist & Internet-arkitekt\n>  * Tlf.: +45   Website: http://xxxxxxxxxxx/\n> \n>  [x] quote me freely  [ ] ask before reusing  [ ] keep private".to_string(),
                ),
                headers: {
                    let mut map = HeaderMap::new();
                    map.insert(HeaderName::TO, "1001084@bugs.debian.org".into());
                    map.insert(HeaderName::IN_REPLY_TO, "<168435582154.540248.1403466294276093439@xxxxxxxxxxxxxx>".into());
                    map.insert(HeaderName::REFERENCES, "<163857393129.1083042.11053317018847169002.reportbug@xxxxxxxxxxxxxx>\n <168435582154.540248.1403466294276093439@xxxxxxxxxxxxxx>".into());
                    map.insert(HeaderName::SUBJECT, "Re: ITP: meli -- terminal mail client".into());
                    map
                },

            });
    }

#[test]
fn test_email_parser_dates_date_new() {
    let s = b"Thu, 31 Aug 2017 13:43:37 +0000 (UTC)";
    let _s = b"Thu, 31 Aug 2017 13:43:37 +0000";
    let __s = b"=?utf-8?q?Thu=2C_31_Aug_2017_13=3A43=3A37_-0000?=";
    assert_eq!(rfc5322_date(s).unwrap(), rfc5322_date(_s).unwrap());
    assert_eq!(rfc5322_date(_s).unwrap(), rfc5322_date(__s).unwrap());
    let val = b"Fri, 23 Dec 0001 21:20:36 -0800 (PST)";
    assert_eq!(rfc5322_date(val).unwrap(), 0);
    let val = b"Wed Sep  9 00:27:54 2020";
    assert_eq!(rfc5322_date(val).unwrap(), 1599611274);
}

#[test]
fn test_email_parser_comment() {
    let s = b"(recursive (comment) block)";
    assert_eq!(comment(s), Ok((&b""[..], ())));
}

#[test]
fn test_email_parser_cfws() {
    let s = r#"This
 is a test"#;
    assert_eq!(&unstructured(s.as_bytes()).unwrap(), "This is a test",);

    assert_eq!(&unstructured(s.as_bytes()).unwrap(), "This is a test",);
    let s = "this is\n\ta folded name";
    assert_eq!(
        &unstructured(s.as_bytes()).unwrap(),
        "this is\ta folded name",
    );
}

#[test]
fn test_email_parser_phrase2() {
    let s = b"\"Jeffrey \\\"fejj\\\" Stedfast\""; // <fejj@helixcode.com>"
    assert_eq!(to_str!(&phrase2(s).unwrap().1), "Jeffrey \"fejj\" Stedfast");
}

#[test]
fn test_email_parser_rfc_2369_list() {
    let s = r#"List-Help: <mailto:list@host.com?subject=help> (List Instructions)
List-Help: <mailto:list-manager@host.com?body=info>
List-Help: <mailto:list-info@host.com> (Info about the list)
List-Help: <http://www.host.com/list/>, <mailto:list-info@host.com>
List-Help: <ftp://ftp.host.com/list.txt> (FTP),
    <mailto:list@host.com?subject=help>
List-Post: <mailto:list@host.com>
List-Post: <mailto:moderator@host.com> (Postings are Moderated)
List-Post: <mailto:moderator@host.com?subject=list%20posting>
List-Post: NO (posting not allowed on this list)
List-Archive: <mailto:archive@host.com?subject=index%20list>
List-Archive: <ftp://ftp.host.com/pub/list/archive/>
List-Archive: <http://www.host.com/list/archive/> (Web Archive)
"#;
    let (rest, headers) = headers::headers(s.as_bytes()).unwrap();
    assert!(rest.is_empty());
    for (_h, v) in headers {
        let (rest, _action_list) = rfc_2369_list_headers_action_list(v).unwrap();
        assert!(rest.is_empty());
    }
}
