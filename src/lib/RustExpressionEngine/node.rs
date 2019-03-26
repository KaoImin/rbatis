use std::collections::HashMap;
use crate::lib::RustExpressionEngine::node::NodeType::{NString, NArg, NNumber, NBool, NNull, NBinary, NOpt};
use serde_json::{Value, Map};
use serde_json::value::Value::{Number, Null};
use serde_json;
use serde_json::de::ParserNumber;
use std::ptr::null;
use crate::lib::RustExpressionEngine::eval::Eval;
use std::fmt::{Display, Formatter, Error};
use crate::lib::RustExpressionEngine::runtime::{IsNumber, OptMap, ParserTokens};
use std::rc::Rc;
use std::sync::Arc;
use serde_json::json;

#[derive(Clone, PartialEq)]
pub enum NodeType {
    NArg = 1,
    //参数节点
    NString = 2,
    //string 节点
    NNumber = 3,
    //number节点
    NBool = 4,
    //bool节点
    NNull = 5,
    //空节点
    NBinary = 6,
    //二元计算节点
    NOpt = 7,           //操作符节点
}

impl Display for NodeType {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        match self {
            NArg => f.write_str("NArg"),
            NString => f.write_str("NString"),
            NNumber => f.write_str("NNumber"),
            NBool => f.write_str("NBool"),
            NNull => f.write_str("NNull"),
            NBinary => f.write_str("NBinary"),
            NOpt => f.write_str("NOpt"),
        }
    }
}


//抽象语法树节点
#[derive(Clone)]
pub struct Node {
    pub value: Value,
    pub leftBinaryNode: Option<Rc<Node>>,
    pub rightBinaryNode: Option<Rc<Node>>,
    pub nodeType: NodeType,
}

impl Node {
    pub fn toNumber(&self) -> f64 {
        return self.value.as_f64().unwrap();
    }
    pub fn toString(&self) -> &str {
        return self.value.as_str().unwrap();
    }
    pub fn toArg(&self) -> &str {
        return self.value.as_str().unwrap();
    }
    pub fn toBool(&self) -> bool {
        return self.value.as_bool().unwrap();
    }
    pub fn toNull(&self) -> () {
        return self.value.as_null().unwrap();
    }
    pub fn toOpt(&self) -> &str {
        return self.value.as_str().unwrap();
    }
    pub fn nodeType(&self) -> NodeType {
        return self.nodeType.clone();
    }

    pub fn equalNodeType(&self, arg: &NodeType) -> bool {
        return self.nodeType == *arg;
    }

    pub fn eval(&self, env: &Value) -> Value {
        if self.equalNodeType(&NBinary) {
            let leftV = self.leftBinaryNode.clone().unwrap().eval(env);
            let rightV = self.rightBinaryNode.clone().unwrap().eval(env);
            let opt = self.toString();
            let (v, _) = Eval(&leftV, &rightV, opt);
            return v;
        } else if self.equalNodeType(&NArg) {
            let arr = &(self.value.as_array().unwrap());
            let arrLen = arr.len() as i32;
            if arrLen == 0 {
                return Value::Null;
            }
            let mut index = 0;
            let mut v = env;
            for item in *arr {
                let itemStr = item.as_str().unwrap();
                v = v.get(itemStr).unwrap_or(&Value::Null);
                if index + 1 == arrLen {
                    return v.clone();
                }
                index = index + 1;
            }
            return Value::Null;
        }
        return self.value.clone();
    }

    pub fn opt(&self) -> Option<&str> {
        return self.value.as_str();
    }


    pub fn newNull() -> Self {
        Self {
            value: Value::Null,
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NNull,
        }
    }
    pub fn newArg(arg: String) -> Self {
        let d: Vec<&str> = arg.split(".").collect();
        Self {
            value: Value::String(arg),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NArg,
        }
    }
    pub fn newString(arg: String) -> Self {
        Self {
            value: Value::String(arg),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NString,
        }
    }
    pub fn newNumberF64(arg: f64) -> Self {
        Self {
            value: Value::Number(serde_json::Number::from(ParserNumber::F64(arg))),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NNumber,
        }
    }
    pub fn newNumberI64(arg: i64) -> Self {
        Self {
            value: Value::Number(serde_json::Number::from(ParserNumber::I64(arg))),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NNumber,
        }
    }
    pub fn newNumberU64(arg: u64) -> Self {
        Self {
            value: Value::Number(serde_json::Number::from(ParserNumber::U64(arg))),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NNumber,
        }
    }

    pub fn newBool(arg: bool) -> Self {
        Self {
            value: Value::Bool(arg),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NBool,
        }
    }
    pub fn newBinary(argLef: Node, argRight: Node, opt: &str) -> Self {
        Self {
            value: Value::from(opt),
            leftBinaryNode: Option::Some(Rc::new(argLef)),
            rightBinaryNode: Option::Some(Rc::new(argRight)),
            nodeType: NBinary,
        }
    }
    pub fn newOpt(arg: String) -> Self {
        Self {
            value: Value::String(arg),
            leftBinaryNode: None,
            rightBinaryNode: None,
            nodeType: NOpt,
        }
    }

    //根据string 解析单个node
    pub fn parser(data: String) -> Self {
        // println!("data={}", &data);
        let dataStr=data.as_str();
        let opt = OptMap::new();
        let mut firstIndex = 0;
        let mut lastIndex = 0;
        if data.rfind("'").unwrap_or(0) != 0 {
            firstIndex = data.find("'").unwrap_or_default();
            lastIndex = data.rfind("'").unwrap_or_default();
        }
        if data.rfind("`").unwrap_or(0) != 0 {
            firstIndex = data.find("`").unwrap_or_default();
            lastIndex = data.rfind("`").unwrap_or_default();
        }
        if dataStr == "" || dataStr == "null" {
            return Node::newNull();
        } else if dataStr == "true" || dataStr == "false" {
            if dataStr == "true" {
                return Node::newBool(true);
            } else {
                return Node::newBool(false);
            }
        } else if opt.isOpt(dataStr) {
            return Node::newOpt(data.clone());
        } else if firstIndex == 0 && lastIndex == (data.len() - 1) && firstIndex != lastIndex {
            let newStr = data.replace("'", "").replace("`", "");
            return Node::newString(newStr);
        } else if IsNumber(&data) {
            if data.find(".").unwrap_or(0) != 0 {
                let parsed = data.parse().unwrap();
                return Node::newNumberF64(parsed);
            } else {
                let parsed = data.parse().unwrap();
                return Node::newNumberI64(parsed);
            }
        } else {
            return Node::newArg(data);
        }
    }
}