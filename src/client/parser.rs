use std::{sync::{Arc}, collections::HashMap, thread, time::Duration, process::Output};

use futures::{join, Future, future::{try_join_all, join_all}};
use log::{info, debug, error};

use crate::{generic::{social_network::{dispatch_social_network_async, SocialNetworkEnum}, parsing_tasks::{ParsingTaskStatus, ParsingTask}}, client::{db::tasks_db::{get_tasks_grouped_by_social_network, GroupedTasks, insert_tasks, update_tasks_with_status}}};

use super::{settings::{Account}, http_client::HttpAuthData, managers::{account_manager::{AccountManager, AccountPtr}, task_manager::TaskManager}};

use std::sync::RwLock;

pub type AccountManagerPtr = Arc<tokio::sync::RwLock<AccountManager>>;

pub type TaskManagerPtr = Arc<tokio::sync::RwLock<TaskManager>>;

pub struct Parser {
    pub account_manager: AccountManagerPtr,
    pub task_manager: TaskManagerPtr
}

impl Parser {

    pub fn new(account_manager: AccountManagerPtr, task_manager: TaskManagerPtr) -> Parser {
        return Parser{
            account_manager,
            task_manager
        }
    }

    pub async fn start(&self) {
        info!("start parsing loop");
        while true {
            self.parse().await;
        }
    }

    async fn parse(&self) {
        let mut account_manager_ptr = self.account_manager.clone();

        let tasks = get_tasks_grouped_by_social_network().await;
        update_tasks_with_status(tasks.iter()
            .flat_map(|item| &item.tasks)
            .filter(|&item| item._id.is_some())
            .map(|item| item._id.unwrap())
            .collect(), 
            ParsingTaskStatus::Processed
        ).await;
        let mut tasks = GroupedTasks::to_hashmap( tasks);

        let social_nets_with_tasks: Vec<SocialNetworkEnum> = tasks.keys().cloned().collect();

        debug!("locking account manager 1");
        let mut account_manager_locked = account_manager_ptr.write().await;
        let mut accounts = account_manager_locked.get_accounts(&social_nets_with_tasks);
        drop(account_manager_locked);

        let unused_tasks = Self::get_unused_tasks(&mut accounts, &mut tasks);

        update_tasks_with_status(unused_tasks.iter()
            .filter(|&item| item._id.is_some())
            .map(|item| item._id.unwrap())
            .collect(),
            ParsingTaskStatus::New
        ).await;


        debug!("parsing num of tasks: {}, num of accounts: {}", tasks.len(), accounts.len());

        let mut parsing_tasks = Vec::new();
        for accounts in accounts.iter_mut() {
            parsing_tasks.push(tokio::spawn(
                Self::parse_tasks(
                    self.account_manager.clone(), 
                    self.task_manager.clone(), 
                    accounts.1.clone(), 
                    tasks.get(accounts.0).unwrap().clone())
                ));
        }

        join_all(parsing_tasks).await;

        thread::sleep(Duration::from_millis(1000));

    }


    async fn parse_tasks(account_manager_ptr: AccountManagerPtr, task_manager_ptr: TaskManagerPtr, account: (AccountPtr, HttpAuthData), tasks_to_parse: Vec<ParsingTask>) {
        info!("start parsing task");
        error!("old auth data {:?}", account.1);
        let (new_auth_data, new_tasks): (Option<HttpAuthData>, Vec<ParsingTask>) = dispatch_social_network_async(
            (account_manager_ptr.clone(), account.clone(), tasks_to_parse),
            account.0.social_network,
            async move |data, network_ptr| {
                return network_ptr.parse(data.0, data.1, data.2).await;
            })
            .await;
        error!("{:?}", new_auth_data);
        if new_auth_data.is_some() {
            //locking
            let http_uw = new_auth_data.unwrap();
            let mut account_manager_locked = account_manager_ptr.write().await;
            //do we really need this?
            account_manager_locked.update_auth_data(account.0.clone(), &http_uw);
            account_manager_locked.add_account(account.0.clone(), http_uw);
        }

        if !new_tasks.is_empty() {
            insert_tasks(&new_tasks).await;
        }
    

    }


    fn get_unused_tasks(accounts: &mut HashMap<SocialNetworkEnum, (Arc<Account>, HttpAuthData)>, tasks: &mut HashMap<SocialNetworkEnum, Vec<ParsingTask>>) -> Vec<ParsingTask> {

        let mut unused_tasks: Vec<ParsingTask> = Vec::with_capacity(10);
        tasks.drain_filter(|social_net, parsing_tasks| {
            return match accounts.contains_key(social_net) {
                true => false,
                false => {
                    unused_tasks.append(parsing_tasks);
                    return true;
                },
            }
        });
        unused_tasks
            .iter_mut()
            .for_each(|item| {
                item.execution_time = item.execution_time + 2000;
            });

        return unused_tasks;
    }

}