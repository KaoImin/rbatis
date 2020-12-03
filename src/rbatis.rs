use std::borrow::BorrowMut;
use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use once_cell::sync::OnceCell;
use serde::de::DeserializeOwned;
use serde::ser::Serialize;
use serde_json::Number;

use crate::core::db::{DriverType, PoolOptions};
use crate::core::db_adapter::{DBExecResult, DBPool, DBPoolConn, DBQuery, DBTx};
use crate::core::Error;
use crate::core::runtime::Arc;
use crate::core::sync::sync_map::SyncMap;
use crate::interpreter::json::runtime::Runtime;
use crate::interpreter::sql::ast::RbatisAST;
use crate::interpreter::sql::node::node::do_child_nodes;
use crate::interpreter::sql::node::node_type::NodeType;
use crate::interpreter::sql::node::proxy_node::CustomNodeGenerate;
use crate::interpreter::sql::py_sql::Py;
use crate::plugin::intercept::SqlIntercept;
use crate::plugin::log::{LogPlugin, RbatisLog};
use crate::plugin::logic_delete::{LogicDelete, RbatisLogicDeletePlugin};
use crate::plugin::page::{IPage, IPageRequest, Page, PagePlugin, RbatisPagePlugin};
use crate::sql::PageLimit;
use crate::tx::{TxManager, TxState, TxGuard};
use crate::utils::error_util::ToResult;
use crate::utils::string_util;
use crate::wrapper::Wrapper;
use uuid::Uuid;

/// rbatis engine
pub struct Rbatis {
    // the connection pool,use OnceCell init this
    pub pool: OnceCell<DBPool>,
    // the engine run some express for example:'1+1'=2
    pub engine: Runtime,
    //py lang
    pub py: Py,
    //tx manager
    pub tx_manager: Arc<TxManager>,
    // page plugin
    pub page_plugin: Box<dyn PagePlugin>,
    // sql intercept vec chain
    pub sql_intercepts: Vec<Box<dyn SqlIntercept>>,
    // logic delete plugin
    pub logic_plugin: Option<Box<dyn LogicDelete>>,
    // log plugin
    pub log_plugin: Arc<Box<dyn LogPlugin>>,
}

impl Default for Rbatis {
    fn default() -> Rbatis {
        Rbatis::new()
    }
}

impl Drop for Rbatis {
    fn drop(&mut self) {
        crate::core::runtime::block_on(async {
            //notice tx manager exit
            self.tx_manager.close().await;
            match self.pool.get_mut() {
                Some(p) => {
                    p.close().await;
                }
                _ => {}
            }
        });
    }
}

///Rbatis Options
pub struct RbatisOption {
    /// the tx lock timeout, if out of time tx will be rollback
    pub tx_lock_wait_timeout: Duration,
    /// rbatis tx manager check tx interval
    pub tx_check_interval: Duration,
    /// custom py lang
    pub generate: Vec<Box<dyn CustomNodeGenerate>>,
    /// page plugin
    pub page_plugin: Box<dyn PagePlugin>,
    /// sql intercept vec chain
    pub sql_intercepts: Vec<Box<dyn SqlIntercept>>,
    /// logic delete plugin
    pub logic_plugin: Option<Box<dyn LogicDelete>>,
    /// log plugin
    pub log_plugin: Arc<Box<dyn LogPlugin>>,
}

impl Default for RbatisOption {
    fn default() -> Self {
        Self {
            tx_lock_wait_timeout: Duration::from_secs(60),
            tx_check_interval: Duration::from_secs(1),
            generate: vec![],
            page_plugin: Box::new(RbatisPagePlugin {}),
            sql_intercepts: vec![],
            logic_plugin: None,
            log_plugin: Arc::new(Box::new(RbatisLog::default()) as Box<dyn LogPlugin>),
        }
    }
}

impl Rbatis {
    ///create an Rbatis
    pub fn new() -> Self {
        return Self::new_with_opt(RbatisOption::default());
    }

    ///new Rbatis from Option
    pub fn new_with_opt(option: RbatisOption) -> Self {
        return Self {
            pool: OnceCell::new(),
            engine: Runtime::new(),
            tx_manager: TxManager::new_arc(option.log_plugin.clone(), option.tx_lock_wait_timeout, option.tx_check_interval),
            page_plugin: option.page_plugin,
            sql_intercepts: option.sql_intercepts,
            logic_plugin: option.logic_plugin,
            log_plugin: option.log_plugin,
            py: Py { cache: Default::default(), generate: option.generate },
        };
    }


    /// try return an new wrapper,if not call the link() method,it will be panic!
    pub fn new_wrapper(&self) -> Wrapper {
        let driver = self.driver_type();
        if driver.as_ref().unwrap().eq(&DriverType::None) {
            panic!("[rbatis] .new_wrapper() method must be call .link(url) to init first!");
        }
        Wrapper::new(&driver.unwrap_or_else(|_| {
            panic!("[rbatis] .new_wrapper() method must be call .link(url) to init first!");
        }))
    }

    /// link pool
    pub async fn link(&self, url: &str) -> Result<(), Error> {
        return Ok(self.link_opt(url, &PoolOptions::default()).await?);
    }

    /// link pool by options
    /// for example:
    pub async fn link_opt(&self, url: &str, opt: &PoolOptions) -> Result<(), Error> {
        if url.is_empty() {
            return Err(Error::from("[rbatis] link url is empty!"));
        }
        if self.pool.get().is_none() {
            let pool = DBPool::new_opt(url, opt).await?;
            self.pool.get_or_init(|| {
                return pool;
            });
        }
        return Ok(());
    }

    pub fn set_log_plugin<T>(&mut self, arg: T) where T: LogPlugin + 'static {
        self.log_plugin = Arc::new(Box::new(arg));
    }

    pub fn set_logic_plugin<T>(&mut self, arg: Option<T>) where T: LogicDelete + 'static {
        match arg {
            Some(v) => {
                self.logic_plugin = Some(Box::new(v));
            }
            None => {
                self.logic_plugin = None;
            }
        }
    }

    pub fn set_page_plugin<T>(&mut self, arg: T) where T: PagePlugin + 'static {
        self.page_plugin = Box::new(arg);
    }

    pub fn add_sql_intercept<T>(&mut self, arg: T) where T: SqlIntercept + 'static {
        self.sql_intercepts.push(Box::new(arg));
    }

    pub fn set_sql_intercepts<T>(&mut self, arg: Vec<Box<dyn SqlIntercept>>) {
        self.sql_intercepts = arg;
    }

    /// get conn pool
    pub fn get_pool(&self) -> Result<&DBPool, Error> {
        let p = self.pool.get();
        if p.is_none() {
            return Err(Error::from("[rbatis] rbatis pool not inited!"));
        }
        return Ok(p.unwrap());
    }

    /// get driver type
    pub fn driver_type(&self) -> Result<DriverType, Error> {
        let pool = self.get_pool()?;
        Ok(pool.driver_type)
    }


    /// begin tx,if TxGuard Drop, tx will be commit(is_drop_commit==true) or rollback(is_drop_commit==false)
    pub async fn begin_tx_defer(&self, drop_commit: bool) -> Result<TxGuard, Error> {
        let tx_id = self.begin_tx().await?;
        let guard = TxGuard::new(&tx_id, drop_commit, self.tx_manager.clone());
        return Ok(guard);
    }

    /// begin tx,for new conn,return (String(context_id/tx_id),u64)
    /// tx_id must be 'tx:'+id,this method default is 'tx:'+uuid
    pub async fn begin_tx(&self) -> Result<String, Error> {
        let new_context_id = format!("tx:{}", Uuid::new_v4().to_string());
        return Ok(self.begin(&new_context_id).await?);
    }

    /// begin tx,for new conn,return <u64(tx num),Error>
    pub async fn begin(&self, context_id: &str) -> Result<String, Error> {
        if context_id.is_empty() {
            return Err(Error::from("[rbatis] context_id can not be empty"));
        }
        if !context_id.starts_with("tx:") {
            return Err(Error::from(format!("[rbatis] context_id: {}  must be start with 'tx:', for example: tx:{}", context_id, context_id)));
        }
        let result = self.tx_manager.begin(context_id, self.get_pool()?).await?;
        return Ok(result);
    }

    /// commit tx,and return conn,return <u64(tx num),Error>
    pub async fn commit(&self, context_id: &str) -> Result<String, Error> {
        if context_id.is_empty() {
            return Err(Error::from("[rbatis] context_id can not be empty"));
        }
        if !context_id.starts_with("tx:") {
            return Err(Error::from(format!("[rbatis] context_id: {} must be start with 'tx:', for example: tx:{}", context_id, context_id)));
        }
        let result = self.tx_manager.commit(context_id).await?;
        return Ok(result);
    }

    /// rollback tx,and return conn,return <u64(tx num),Error>
    pub async fn rollback(&self, context_id: &str) -> Result<String, Error> {
        if context_id.is_empty() {
            return Err(Error::from("[rbatis] context_id can not be empty"));
        }
        if !context_id.starts_with("tx:") {
            return Err(Error::from(format!("[rbatis] context_id: {} must be start with 'tx:', for example: tx:{}", context_id, context_id)));
        }
        let result = self.tx_manager.rollback(context_id).await?;
        return Ok(result);
    }


    /// fetch result(row sql)
    pub async fn fetch<T>(&self, context_id: &str, sql: &str) -> Result<T, Error>
        where T: DeserializeOwned {

        //sql intercept
        let mut sql = sql.to_string();
        for item in &self.sql_intercepts {
            item.do_intercept(self, &mut sql, &mut vec![], false);
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] Query ==> {}", context_id, sql.as_str()));
        }
        let result;
        let mut fetch_num = 0;
        if context_id.starts_with("tx:") {
            let conn = self.tx_manager.get_mut(context_id).await;
            if conn.is_none() {
                return Err(Error::from(format!("[rbatis] tx:{} not exist！", context_id)));
            }
            let mut conn = conn.unwrap();
            let (data, num) = conn.value_mut().0.fetch(sql.as_str()).await?;
            result = data;
            fetch_num = num;
        } else {
            let mut conn = self.get_pool()?.acquire().await?;
            let (data, num) = conn.fetch(sql.as_str()).await?;
            result = data;
            fetch_num = num;
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] ReturnRows <== {}", context_id, fetch_num));
        }
        return Ok(result);
    }

    /// exec sql(row sql)
    pub async fn exec(&self, context_id: &str, sql: &str) -> Result<DBExecResult, Error> {

        //sql intercept
        let mut sql = sql.to_string();
        for item in &self.sql_intercepts {
            item.do_intercept(self, &mut sql, &mut vec![], false);
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] Exec ==> :{}", context_id, &sql));
        }
        let data;
        if context_id.starts_with("tx:") {
            let conn = self.tx_manager.get_mut(context_id).await;
            if conn.is_none() {
                return Err(Error::from(format!("[rbatis] tx:{} not exist！", context_id)));
            }
            let mut conn = conn.unwrap();
            data = conn.value_mut().0.execute(&sql).await?;
        } else {
            let mut conn = self.get_pool()?.acquire().await?;
            data = conn.execute(&sql).await?;
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] RowsAffected <== {}", context_id, &data.rows_affected));
        }
        return Ok(data);
    }


    /// bind arg into DBQuery
    fn bind_arg<'arg>(&self, sql: &'arg str, arg: &Vec<serde_json::Value>) -> Result<DBQuery<'arg>, Error> {
        let mut q: DBQuery = self.get_pool()?.make_query(sql)?;
        for x in arg {
            q.bind_value(x);
        }
        return Ok(q);
    }

    /// fetch result(prepare sql)
    pub async fn fetch_prepare<T>(&self, context_id: &str, sql: &str, args: &Vec<serde_json::Value>) -> Result<T, Error>
        where T: DeserializeOwned {

        //sql intercept
        let mut sql = sql.to_string();
        let mut args = args.clone();
        for item in &self.sql_intercepts {
            item.do_intercept(self, &mut sql, &mut args, true);
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] Query ==> {}\n{}[rbatis] [{}] Args  ==> {}", context_id, &sql, string_util::LOG_SPACE, context_id, serde_json::Value::Array(args.clone()).to_string()));
        }
        let result_data;
        let mut return_num = 0;
        if context_id.starts_with("tx:") {
            let q: DBQuery = self.bind_arg(&sql, &args)?;
            let conn = self.tx_manager.get_mut(context_id).await;
            if conn.is_none() {
                return Err(Error::from(format!("[rbatis] tx:{} not exist！", context_id)));
            }
            let mut conn = conn.unwrap();
            let (result, num) = conn.value_mut().0.fetch_parperd(q).await?;
            result_data = result;
            return_num = num;
        } else {
            let mut conn = self.get_pool()?.acquire().await?;
            let q: DBQuery = self.bind_arg(&sql, &args)?;
            let (result, num) = conn.fetch_parperd(q).await?;
            result_data = result;
            return_num = num;
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] ReturnRows <== {}", context_id, return_num));
        }
        return Ok(result_data);
    }

    /// exec sql(prepare sql)
    pub async fn exec_prepare(&self, context_id: &str, sql: &str, args: &Vec<serde_json::Value>) -> Result<DBExecResult, Error> {

        //sql intercept
        let mut sql = sql.to_string();
        let mut args = args.clone();
        for item in &self.sql_intercepts {
            item.do_intercept(self, &mut sql, &mut args, true);
        }
        if self.log_plugin.is_enable() {
            self.log_plugin.do_log(&format!("[rbatis] [{}] Exec  ==> {}\n{}[rbatis] [{}] Args  ==> {}", context_id, &sql, string_util::LOG_SPACE, context_id, serde_json::Value::Array(args.clone()).to_string()));
        }
        let result;
        if context_id.starts_with("tx:") {
            let q: DBQuery = self.bind_arg(&sql, &args)?;
            let conn = self.tx_manager.get_mut(context_id).await;
            if conn.is_none() {
                return Err(Error::from(format!("[rbatis] tx:{} not exist！", context_id)));
            }
            let mut conn = conn.unwrap();
            result = conn.value_mut().0.exec_prepare(q).await;
        } else {
            let q: DBQuery = self.bind_arg(&sql, &args)?;
            let mut conn = self.get_pool()?.acquire().await?;
            result = conn.exec_prepare(q).await;
        }
        if self.log_plugin.is_enable() {
            if result.is_ok() {
                self.log_plugin.do_log(&format!("[rbatis] [{}] RowsAffected <== {:#?}", context_id, result.as_ref()));
            } else {
                self.log_plugin.do_log(&format!("[rbatis] [{}] RowsAffected <== {}", context_id, 0));
            }
        }
        return result;
    }

    pub async fn fetch_prepare_wrapper<T>(&self, context_id: &str, w: &Wrapper) -> Result<T, Error>
        where T: DeserializeOwned {
        let w = w.clone().check()?;
        self.fetch_prepare(context_id, w.sql.as_str(), &w.args).await
    }

    pub async fn exec_prepare_wrapper(&self, context_id: &str, w: &Wrapper) -> Result<DBExecResult, Error> {
        let w = w.clone().check()?;
        self.exec_prepare(context_id, w.sql.as_str(), &w.args).await
    }

    /// py str into py ast,run get sql,arg result
    fn py_to_sql(&self, py: &str, arg: &serde_json::Value) -> Result<(String, Vec<serde_json::Value>), Error> {
        let nodes = self.py.parse_and_cache(py)?;
        let mut arg_array = vec![];
        let mut env = arg.clone();
        let driver_type = Box::new(self.driver_type()?);
        let mut sql = do_child_nodes(&driver_type, &nodes, &mut env, &self.engine, &mut arg_array)?;
        sql = sql.trim().to_string();
        return Ok((sql, arg_array));
    }

    /// fetch query result(prepare sql)
    ///for example:
    ///
    ///         let py = r#"
    ///     SELECT * FROM biz_activity
    ///    WHERE delete_flag = #{delete_flag}
    ///     if name != null:
    ///       AND name like #{name+'%'}
    ///     if ids != null:
    ///       AND id in (
    ///       trim ',':
    ///          for item in ids:
    ///            #{item},
    ///       )"#;
    ///         let data: serde_json::Value = rb.py_fetch("", py, &json!({   "delete_flag": 1 })).await.unwrap();
    ///
    pub async fn py_fetch<T, Ser>(&self, context_id: &str, py: &str, arg: &Ser) -> Result<T, Error>
        where T: DeserializeOwned,
              Ser: Serialize + Send + Sync {
        let json = json!(arg);
        let (sql, args) = self.py_to_sql(py, &json)?;
        return self.fetch_prepare(context_id, sql.as_str(), &args).await;
    }

    /// exec sql(prepare sql)
    ///for example:
    ///
    ///         let py = r#"
    ///     SELECT * FROM biz_activity
    ///    WHERE delete_flag = #{delete_flag}
    ///     if name != null:
    ///       AND name like #{name+'%'}
    ///     if ids != null:
    ///       AND id in (
    ///       trim ',':
    ///          for item in ids:
    ///            #{item},
    ///       )"#;
    ///         let data: u64 = rb.py_exec("", py, &json!({   "delete_flag": 1 })).await.unwrap();
    ///
    pub async fn py_exec<Ser>(&self, context_id: &str, py: &str, arg: &Ser) -> Result<DBExecResult, Error>
        where Ser: Serialize + Send + Sync {
        let json = json!(arg);
        let (sql, args) = self.py_to_sql(py, &json)?;
        return self.exec_prepare(context_id, sql.as_str(), &args).await;
    }

    /// fetch page result(prepare sql)
    pub async fn fetch_page<T>(&self, context_id: &str, sql: &str, args: &Vec<serde_json::Value>, page_request: &dyn IPageRequest) -> Result<Page<T>, Error>
        where T: DeserializeOwned + Serialize + Send + Sync {
        let mut page_result = Page::new(page_request.get_current(), page_request.get_size());
        let (count_sql, sql) = self.page_plugin.make_page_sql(&self.driver_type()?, context_id, sql, args, page_request)?;
        if page_request.is_serch_count() {
            //make count sql
            let total: Option<u64> = self.fetch_prepare(context_id, count_sql.as_str(), args).await?;
            page_result.set_total(total.unwrap_or(0));
            page_result.pages = page_result.get_pages();
            if page_result.get_total() == 0 {
                return Ok(page_result);
            }
        }
        let data: Option<Vec<T>> = self.fetch_prepare(context_id, sql.as_str(), args).await?;
        page_result.set_records(data.unwrap_or(vec![]));
        page_result.pages = page_result.get_pages();
        return Ok(page_result);
    }

    /// fetch result(prepare sql)
    pub async fn py_fetch_page<T, Ser>(&self, context_id: &str, py: &str, arg: &Ser, page: &dyn IPageRequest) -> Result<Page<T>, Error>
        where T: DeserializeOwned + Serialize + Send + Sync,
              Ser: Serialize + Send + Sync {
        let json = json!(arg);
        let (sql, args) = self.py_to_sql(py, &json)?;
        return self.fetch_page::<T>(context_id, sql.as_str(), &args, page).await;
    }
}