use std::cmp::PartialEq;
use std::time::Instant;

use async_trait::async_trait;
use futures::sync::mpsc::Sender;
use futures03::channel::oneshot::channel;

use graph::blockchain::RuntimeAdapter;
use graph::blockchain::{Blockchain, DataSource};
use graph::blockchain::{HostFn, TriggerWithHandler};
use graph::components::store::{EnsLookup, SubgraphFork};
use graph::components::subgraph::{MappingError, SharedProofOfIndexing};
use graph::prelude::{
    RuntimeHost as RuntimeHostTrait, RuntimeHostBuilder as RuntimeHostBuilderTrait, *,
};

use crate::mapping::{MappingContext, MappingRequest};
use crate::{host_exports::HostExports, module::ExperimentalFeatures};
use graph::runtime::gas::Gas;

pub struct RuntimeHostBuilder<C: Blockchain> {
    runtime_adapter: Arc<dyn RuntimeAdapter<C>>,
    link_resolver: Arc<dyn LinkResolver>,
    ens_lookup: Arc<dyn EnsLookup>,
}

impl<C: Blockchain> Clone for RuntimeHostBuilder<C> {
    fn clone(&self) -> Self {
        RuntimeHostBuilder {
            runtime_adapter: self.runtime_adapter.cheap_clone(),
            link_resolver: self.link_resolver.cheap_clone(),
            ens_lookup: self.ens_lookup.cheap_clone(),
        }
    }
}

impl<C: Blockchain> RuntimeHostBuilder<C> {
    pub fn new(
        runtime_adapter: Arc<dyn RuntimeAdapter<C>>,
        link_resolver: Arc<dyn LinkResolver>,
        ens_lookup: Arc<dyn EnsLookup>,
    ) -> Self {
        RuntimeHostBuilder {
            runtime_adapter,
            link_resolver,
            ens_lookup,
        }
    }
}

impl<C: Blockchain> RuntimeHostBuilderTrait<C> for RuntimeHostBuilder<C> {
    type Host = RuntimeHost<C>;
    type Req = MappingRequest<C>;

    fn spawn_mapping(
        raw_module: &[u8],
        logger: Logger,
        subgraph_id: DeploymentHash,
        metrics: Arc<HostMetrics>,
    ) -> Result<Sender<Self::Req>, Error> {
        let experimental_features = ExperimentalFeatures {
            allow_non_deterministic_ipfs: ENV_VARS.mappings.allow_non_deterministic_ipfs,
        };
        crate::mapping::spawn_module(
            raw_module,
            logger,
            subgraph_id,
            metrics,
            tokio::runtime::Handle::current(),
            ENV_VARS.mappings.timeout,
            experimental_features,
        )
    }

    fn build(
        &self,
        network_name: String,
        subgraph_id: DeploymentHash,
        data_source: C::DataSource,
        templates: Arc<Vec<C::DataSourceTemplate>>,
        mapping_request_sender: Sender<MappingRequest<C>>,
        metrics: Arc<HostMetrics>,
    ) -> Result<Self::Host, Error> {
        RuntimeHost::new(
            self.runtime_adapter.cheap_clone(),
            self.link_resolver.clone(),
            network_name,
            subgraph_id,
            data_source,
            templates,
            mapping_request_sender,
            metrics,
            self.ens_lookup.cheap_clone(),
        )
    }
}

pub struct RuntimeHost<C: Blockchain> {
    host_fns: Arc<Vec<HostFn>>,
    data_source: C::DataSource,
    mapping_request_sender: Sender<MappingRequest<C>>,
    // mapping_requests_sender: Sender<MappingRequests<C>>,
    host_exports: Arc<HostExports<C>>,
    metrics: Arc<HostMetrics>,
}

impl<C> RuntimeHost<C>
where
    C: Blockchain,
{
    fn new(
        runtime_adapter: Arc<dyn RuntimeAdapter<C>>,
        link_resolver: Arc<dyn LinkResolver>,
        network_name: String,
        subgraph_id: DeploymentHash,
        data_source: C::DataSource,
        templates: Arc<Vec<C::DataSourceTemplate>>,
        mapping_request_sender: Sender<MappingRequest<C>>,
        metrics: Arc<HostMetrics>,
        ens_lookup: Arc<dyn EnsLookup>,
    ) -> Result<Self, Error> {
        // Create new instance of externally hosted functions invoker. The `Arc` is simply to avoid
        // implementing `Clone` for `HostExports`.
        let host_exports = Arc::new(HostExports::new(
            subgraph_id,
            &data_source,
            network_name,
            templates,
            link_resolver,
            ens_lookup,
        ));

        let host_fns = Arc::new(runtime_adapter.host_fns(&data_source)?);

        Ok(RuntimeHost {
            host_fns,
            data_source,
            mapping_request_sender,
            host_exports,
            metrics,
        })
    }

    /// Sends a MappingRequest to the thread which owns the host,
    /// and awaits the result.
    async fn send_mapping_request(
        &self,
        logger: &Logger,
        state: BlockState<C>,
        triggers: Vec<TriggerWithHandler<C>>,
        block_ptr: BlockPtr,
        proof_of_indexing: SharedProofOfIndexing,
        debug_fork: &Option<Arc<dyn SubgraphFork>>,
    ) -> Result<BlockState<C>, MappingError> {
        let handler = triggers[0].handler_name().to_string();

        let extras = triggers[0].logging_extras();
        trace!(
            logger, "Start processing trigger";
            &extras,
            "handler" => &handler,
            "data_source" => &self.data_source.name(),
        );

        let (result_sender, result_receiver) = channel();
        let start_time = Instant::now();
        let metrics = self.metrics.clone();

        self.mapping_request_sender
            .clone()
            .send(MappingRequest {
                ctx: MappingContext {
                    logger: logger.cheap_clone(),
                    state,
                    host_exports: self.host_exports.cheap_clone(),
                    block_ptr,
                    proof_of_indexing,
                    host_fns: self.host_fns.cheap_clone(),
                    debug_fork: debug_fork.cheap_clone(),
                },
                triggers,
                result_sender,
            })
            .compat()
            .await
            .context("Mapping terminated before passing in trigger")?;
        
        
        
        let mut result = result_receiver
            .await
            .context("Mapping terminated before handling trigger")?;
            

        println!("------------block_state results size:{}-------------------",result.len());
        
       /* for i in 0..result.len(){
            println!("{}",i);
            let resut_copy=result[i].as_ref().unwrap().clone();
            println!("{:?}",resut_copy.0.entity_cache);
        }*/

        println!("------------END block_state results-------------------");
        let elapsed = start_time.elapsed();
        metrics.observe_handler_execution_time(elapsed.as_secs_f64(), &handler);
        let mut final_state_res=result.pop().unwrap();
        let gas_used = final_state_res.as_ref().map(|(_, gas)| gas).unwrap_or(&Gas::ZERO);
        info!(
            logger, "Done processing trigger";
            &extras,
            "total_ms" => elapsed.as_millis(),
            "handler" => handler.clone(),
            "data_source" => &self.data_source.name(),
            "gas_used" => gas_used.to_string(),
        );
        while !result.is_empty()
        {
            //let next_state_res=result.pop().unwrap();
            let block_state_res=result.pop().unwrap();
            let final_state=final_state_res.as_mut().unwrap();
            let next_state=block_state_res.unwrap();
            final_state.0.combine(next_state.0);
            final_state.1+=next_state.1;
        }
        println!("------------final_state results-------------------");
        //let resut_copy=final_state_res.as_ref().unwrap().clone();
        //println!("{:?}",resut_copy.0.entity_cache);
        let gas_used = final_state_res.as_ref().map(|(_, gas)| gas).unwrap_or(&Gas::ZERO);
            info!(
                logger, "Done processing trigger";
                &extras,
                "total_ms" => elapsed.as_millis(),
                "handler" => handler.clone(),
                "data_source" => &self.data_source.name(),
                "gas_used" => gas_used.to_string(),
            );
        final_state_res.map(|(block_state, _)| block_state)
        /*let block_state_res=result.pop().unwrap();
        // If there is an error, "gas_used" is incorrectly reported as 0.
        let gas_used = block_state_res.as_ref().map(|(_, gas)| gas).unwrap_or(&Gas::ZERO);
        info!(
            logger, "Done processing trigger";
            &extras,
            "total_ms" => elapsed.as_millis(),
            "handler" => handler,
            "data_source" => &self.data_source.name(),
            "gas_used" => gas_used.to_string(),
        );

        // Discard the gas value
        block_state_res.map(|(block_state, _)| block_state)*/
    }




}

#[async_trait]
impl<C: Blockchain> RuntimeHostTrait<C> for RuntimeHost<C> {
    fn match_and_decode(
        &self,
        trigger: &C::TriggerData,
        block: &Arc<C::Block>,
        logger: &Logger,
    ) -> Result<Option<TriggerWithHandler<C>>, Error> {
        self.data_source.match_and_decode(trigger, block, logger)
    }

    async fn process_mapping_trigger(
        &self,
        logger: &Logger,
        block_ptr: BlockPtr,
        trigger: Vec<TriggerWithHandler<C>>,
        state: BlockState<C>,
        proof_of_indexing: SharedProofOfIndexing,
        debug_fork: &Option<Arc<dyn SubgraphFork>>,
    ) -> Result<BlockState<C>, MappingError> {
        self.send_mapping_request(
            logger,
            state,
            trigger,
            block_ptr,
            proof_of_indexing,
            debug_fork,
        )
        .await
    }

    fn creation_block_number(&self) -> Option<BlockNumber> {
        self.data_source.creation_block()
    }
}

impl<C: Blockchain> PartialEq for RuntimeHost<C> {
    fn eq(&self, other: &Self) -> bool {
        self.data_source.is_duplicate_of(&other.data_source)
    }
}
